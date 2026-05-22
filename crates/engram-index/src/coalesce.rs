//! Request coalescer — dedupe in-flight identical fetches within a short window.
//!
//! When the council convenes 3-5 agents on the same note, each agent calls the
//! same retrieval functions (`read_note`, `hybrid_search`, `list_neighbors`).
//! Without coalescing those are 3-5× duplicated calls. The coalescer serves all
//! callers that arrive while a fetch is in flight from one underlying result;
//! after the result lands, the key remains coalescable for a configurable
//! window so closely-trailing requests share the same response.
//!
//! See `docs/design/03-architecture.md` §Request coalescing (council retrieval)
//! for the surrounding design and the table of coalesced calls.
//!
//! # Failure semantics
//!
//! The coalescer is generic over `V: Clone`. If the leader's fetcher panics or
//! is cancelled before completing, followers receive `None` and may retry
//! directly. Callers that need to propagate typed errors should make `V` their
//! own `Result<T, E>` — the coalescer treats the value opaquely.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

/// A short-lived in-memory request deduplicator at a retrieval-API boundary.
///
/// `K` is the request key (e.g. `note_id`, `(query_hash, n)`); `V` is the
/// fetched value. Identical requests within `window` share one fetch.
///
/// Clone is cheap (an `Arc` bump); pass clones to tasks that need to share
/// the same coalescer.
pub struct RequestCoalescer<K, V> {
    inner: Arc<Inner<K, V>>,
}

struct Inner<K, V> {
    in_flight: Mutex<HashMap<K, watch::Sender<Option<V>>>>,
    window: Duration,
    metrics: Mutex<Metrics>,
}

/// Coalesce hit/miss counters.
///
/// A *miss* is a fetch that was actually executed (leader); a *hit* is a call
/// served from an in-flight or recently-completed shared result (follower).
#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics {
    pub hits: u64,
    pub misses: u64,
}

impl<K, V> Clone for RequestCoalescer<K, V> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<K, V> RequestCoalescer<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Construct a coalescer that holds completed entries coalescable for
    /// `window` after the fetch lands. Choose `window` per call type — 50ms
    /// for sqlite reads, 200ms for `hybrid_search`, 1s for `read_index()`.
    pub fn new(window: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                in_flight: Mutex::new(HashMap::new()),
                window,
                metrics: Mutex::new(Metrics::default()),
            }),
        }
    }

    /// Run `fetcher` if no identical request is in flight; otherwise wait for
    /// the in-flight (or recently-completed) result and return a clone.
    ///
    /// Returns `None` only if the leader's fetcher panicked or was cancelled
    /// before producing a value — callers may treat this as a miss and retry.
    pub async fn coalesce<F, Fut>(&self, key: K, fetcher: F) -> Option<V>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = V>,
    {
        let (is_leader, slot) = {
            let mut in_flight = self.inner.in_flight.lock().unwrap();
            if let Some(tx) = in_flight.get(&key) {
                self.inner.metrics.lock().unwrap().hits += 1;
                (false, tx.clone())
            } else {
                let (tx, _rx) = watch::channel::<Option<V>>(None);
                in_flight.insert(key.clone(), tx.clone());
                self.inner.metrics.lock().unwrap().misses += 1;
                (true, tx)
            }
        };

        if is_leader {
            // Schedule key removal even if the fetcher panics or is cancelled.
            let _guard = LeaderGuard::new(Arc::clone(&self.inner), key);
            // A live receiver must exist at the moment of `send`, otherwise
            // `watch::Sender::send` returns `SendError` *and* the current
            // value is never updated — late-arriving followers would then
            // observe `None` forever (until the slot is evicted). The
            // channel's initial `_rx` from `watch::channel` dropped at the
            // end of the registration block above; this keep-alive ensures
            // there is at least one receiver while we send.
            let _keepalive = slot.subscribe();
            let value = fetcher().await;
            let _ = slot.send(Some(value.clone()));
            Some(value)
        } else {
            // Subscribe, then drop our `Sender` clone — otherwise the follower
            // would itself keep the channel open and `rx.changed()` could not
            // observe sender-side drop (used to detect a panicked leader).
            let mut rx = slot.subscribe();
            drop(slot);
            loop {
                let snapshot = rx.borrow_and_update().clone();
                if let Some(v) = snapshot {
                    return Some(v);
                }
                if rx.changed().await.is_err() {
                    // All senders dropped before sending — leader panicked
                    // or was cancelled.
                    return None;
                }
            }
        }
    }

    /// Snapshot of cumulative hit/miss counts.
    pub fn metrics(&self) -> Metrics {
        *self.inner.metrics.lock().unwrap()
    }

    /// Number of keys currently coalescable. Diagnostic only.
    pub fn in_flight_len(&self) -> usize {
        self.inner.in_flight.lock().unwrap().len()
    }
}

/// Drop guard that schedules key removal after the window expires.
///
/// Held by the leader for the duration of its fetch; on drop (normal return,
/// panic, or task cancellation) it spawns a delayed cleanup task.
struct LeaderGuard<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    inner: Arc<Inner<K, V>>,
    key: Option<K>,
}

impl<K, V> LeaderGuard<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn new(inner: Arc<Inner<K, V>>, key: K) -> Self {
        Self {
            inner,
            key: Some(key),
        }
    }
}

impl<K, V> Drop for LeaderGuard<K, V>
where
    K: Eq + Hash + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let Some(key) = self.key.take() else { return };
        let inner = Arc::clone(&self.inner);
        let window = inner.window;
        // `tokio::spawn` requires an active runtime; the leader was running on
        // one, so the runtime is in scope. Fall back to a synchronous remove
        // if Drop runs outside any runtime (e.g., a sync test scaffold).
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn(async move {
                    tokio::time::sleep(window).await;
                    inner.in_flight.lock().unwrap().remove(&key);
                });
            }
            Err(_) => {
                inner.in_flight.lock().unwrap().remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;
    use tokio::time::{sleep, Duration};

    /// Five concurrent identical-key calls trigger exactly one fetch.
    #[tokio::test]
    async fn concurrent_identical_keys_share_one_fetch() {
        let coalescer = RequestCoalescer::<&'static str, u64>::new(Duration::from_millis(50));
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(5));

        let mut handles = Vec::new();
        for _ in 0..5 {
            let c = coalescer.clone();
            let fc = Arc::clone(&fetch_count);
            let b = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                b.wait().await;
                c.coalesce("note-A", move || async move {
                    fc.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_millis(20)).await;
                    42_u64
                })
                .await
            }));
        }

        let results: Vec<_> = futures_join(handles).await;
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);
        assert!(results.iter().all(|v| *v == Some(42)));

        let m = coalescer.metrics();
        assert_eq!(m.misses, 1);
        assert_eq!(m.hits, 4);
    }

    /// Five concurrent distinct-key calls each trigger their own fetch.
    #[tokio::test]
    async fn concurrent_distinct_keys_run_independently() {
        let coalescer = RequestCoalescer::<u32, u32>::new(Duration::from_millis(50));
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for i in 0..5_u32 {
            let c = coalescer.clone();
            let fc = Arc::clone(&fetch_count);
            handles.push(tokio::spawn(async move {
                c.coalesce(i, move || async move {
                    fc.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_millis(10)).await;
                    i * 10
                })
                .await
            }));
        }

        let results: Vec<_> = futures_join(handles).await;
        assert_eq!(fetch_count.load(Ordering::SeqCst), 5);
        let mut got: Vec<u32> = results.iter().map(|r| r.unwrap()).collect();
        got.sort_unstable();
        assert_eq!(got, vec![0, 10, 20, 30, 40]);

        let m = coalescer.metrics();
        assert_eq!(m.misses, 5);
        assert_eq!(m.hits, 0);
    }

    /// A follower that arrives after the leader completes — but inside the
    /// window — still gets the cached value.
    #[tokio::test]
    async fn window_holds_completed_result() {
        let coalescer =
            RequestCoalescer::<&'static str, &'static str>::new(Duration::from_millis(200));
        let fetch_count = Arc::new(AtomicUsize::new(0));

        // First call — leader executes immediately.
        let fc = Arc::clone(&fetch_count);
        let v1 = coalescer
            .coalesce("k", move || async move {
                fc.fetch_add(1, Ordering::SeqCst);
                "first"
            })
            .await;
        assert_eq!(v1, Some("first"));
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);

        // Wait a beat — still well inside the 200ms window.
        sleep(Duration::from_millis(20)).await;

        // Second call — should hit the cached watch slot.
        let fc = Arc::clone(&fetch_count);
        let v2 = coalescer
            .coalesce("k", move || async move {
                fc.fetch_add(1, Ordering::SeqCst);
                "second"
            })
            .await;
        assert_eq!(v2, Some("first"), "follower must see leader's value");
        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "no second fetch inside window"
        );
    }

    /// After the window expires, a fresh call triggers a new fetch.
    #[tokio::test]
    async fn fresh_fetch_after_window_expiry() {
        let coalescer = RequestCoalescer::<&'static str, u32>::new(Duration::from_millis(30));
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let fc = Arc::clone(&fetch_count);
        let v1 = coalescer
            .coalesce("k", move || async move {
                fc.fetch_add(1, Ordering::SeqCst);
                1
            })
            .await;
        assert_eq!(v1, Some(1));

        // Wait past the window. Cleanup tokio::spawn must have time to fire.
        sleep(Duration::from_millis(100)).await;

        let fc = Arc::clone(&fetch_count);
        let v2 = coalescer
            .coalesce("k", move || async move {
                fc.fetch_add(1, Ordering::SeqCst);
                2
            })
            .await;
        assert_eq!(v2, Some(2));
        assert_eq!(fetch_count.load(Ordering::SeqCst), 2);
        assert_eq!(coalescer.in_flight_len(), 1, "new key in flight");
    }

    /// If the leader's fetcher panics, followers receive `None` and the key
    /// is eventually evicted so the next request can retry.
    #[tokio::test]
    async fn leader_panic_does_not_poison_the_map() {
        let coalescer = RequestCoalescer::<&'static str, u32>::new(Duration::from_millis(30));

        let leader = {
            let c = coalescer.clone();
            tokio::spawn(async move {
                c.coalesce("k", || async {
                    panic!("boom");
                    #[allow(unreachable_code)]
                    0_u32
                })
                .await
            })
        };

        // Give the leader a moment to register before we follow.
        sleep(Duration::from_millis(5)).await;
        let follower_result = coalescer
            .coalesce("k", || async { unreachable!("leader holds the slot") })
            .await;

        let leader_outcome = leader.await;
        assert!(leader_outcome.is_err(), "leader task panicked as expected");
        assert_eq!(follower_result, None, "follower sees None on leader panic");

        // Wait past the window for the drop-guard cleanup.
        sleep(Duration::from_millis(100)).await;
        assert_eq!(coalescer.in_flight_len(), 0, "key evicted after window");
    }

    /// Council scenario — five agents query the same neighbors concurrently;
    /// exactly one underlying retrieval fires.
    #[tokio::test]
    async fn council_scenario_dedupes_neighbor_query() {
        let coalescer =
            RequestCoalescer::<(String, u32), Vec<String>>::new(Duration::from_millis(100));
        let underlying_calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(5));

        let agents = vec!["synth", "devil", "linker", "carto", "voice"];
        let mut handles = Vec::new();
        for _ in agents {
            let c = coalescer.clone();
            let calls = Arc::clone(&underlying_calls);
            let b = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                b.wait().await;
                c.coalesce(("note-target".to_string(), 2_u32), move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    sleep(Duration::from_millis(15)).await;
                    vec!["n1".to_string(), "n2".to_string()]
                })
                .await
            }));
        }

        let results: Vec<_> = futures_join(handles).await;
        assert_eq!(
            underlying_calls.load(Ordering::SeqCst),
            1,
            "exactly one underlying retrieval"
        );
        let expected = ["n1".to_string(), "n2".to_string()];
        assert!(results.iter().all(|r| r.as_deref() == Some(&expected[..])));
    }

    // Small join helper to avoid pulling `futures` as a dep just for tests.
    async fn futures_join<T: Send + 'static>(handles: Vec<tokio::task::JoinHandle<T>>) -> Vec<T> {
        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            out.push(h.await.expect("task panicked"));
        }
        out
    }
}
