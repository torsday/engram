//! Integration tests for [`engram_llm::early_exit`].
//!
//! Covers the acceptance criteria from #23:
//!
//! - Incremental parser extracts `confidence` from a streamed JSON body
//!   even when split mid-number across chunks;
//! - Stream is cancelled and `Error::EarlyExit` returned when confidence
//!   is below the configured floor;
//! - Non-trigger path completes normally with the assembled outcome;
//! - `output_tokens_saved_estimated` is positive on early-exit.

use std::pin::Pin;
use std::task::{Context, Poll};

use engram_llm::{
    early_exit_drive, Cost, EarlyExitConfig, EarlyExitStream, Error, StreamChunk,
    StreamedCompletion, Usage,
};
use futures_util::Stream;

/// A scriptable mock stream that yields the supplied chunks in order.
/// Used by every test below; lives here rather than in the lib so the
/// production crate stays free of test-only types.
struct MockStream {
    chunks: Vec<StreamChunk>,
}

impl Stream for MockStream {
    type Item = engram_llm::Result<StreamChunk>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.chunks.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Ok(self.chunks.remove(0))))
        }
    }
}

fn boxed(chunks: Vec<StreamChunk>) -> StreamedCompletion {
    Box::pin(MockStream { chunks })
}

fn delta(s: &str) -> StreamChunk {
    StreamChunk::Delta {
        text: s.to_string(),
    }
}

fn done() -> StreamChunk {
    StreamChunk::Done {
        usage: Usage::default(),
        cost: Cost::unknown(),
        model_used: "mock/echo".to_string(),
        latency_ms: 0,
    }
}

#[tokio::test]
async fn early_exit_drive_cancels_on_low_confidence_split_across_chunks() {
    // Three small deltas that together form `{"confidence": 0.2, ...`.
    // The number `0.2` is split across the second and third chunks to
    // exercise the "needs more" path through the parser.
    let stream = boxed(vec![
        delta(r#"{"confidence": "#),
        delta("0.2"),
        delta(r#", "rationale": "..."}"#),
        done(),
    ]);

    let outcome = early_exit_drive(stream, EarlyExitConfig::new(0.3)).await;
    match outcome {
        Err(Error::EarlyExit {
            confidence,
            floor,
            partial,
            output_tokens_saved_estimated,
        }) => {
            assert!((confidence - 0.2).abs() < f32::EPSILON, "got {confidence}");
            assert!((floor - 0.3).abs() < f32::EPSILON);
            assert!(partial.contains("confidence"), "partial: {partial:?}");
            assert!(output_tokens_saved_estimated > 0);
        }
        other => panic!("expected EarlyExit, got {other:?}"),
    }
}

#[tokio::test]
async fn early_exit_drive_passes_through_when_confidence_above_floor() {
    let stream = boxed(vec![
        delta(r#"{"confidence": 0.85, "#),
        delta(r#""rationale": "..."}"#),
        done(),
    ]);

    let outcome = early_exit_drive(stream, EarlyExitConfig::new(0.3))
        .await
        .expect("expected Ok");
    assert!(outcome.text.contains("0.85"));
    assert_eq!(outcome.model_used, "mock/echo");
}

#[tokio::test]
async fn early_exit_drive_completes_normally_when_field_missing() {
    // No `confidence` field at all — the wrapper must not block forever;
    // it should pass through to the terminal `Done`.
    let stream = boxed(vec![delta(r#"{"rationale": "..."}"#), done()]);

    let outcome = early_exit_drive(stream, EarlyExitConfig::default())
        .await
        .expect("expected Ok");
    assert!(outcome.text.contains("rationale"));
}

#[tokio::test]
async fn early_exit_drive_floor_zero_never_triggers() {
    // floor = 0.0 means "anything ≥ 0.0 passes"; even a confidence of 0.0
    // doesn't cross the *strict* < comparison.
    let stream = boxed(vec![
        delta(r#"{"confidence": 0.0, "rationale": "..."}"#),
        done(),
    ]);

    let outcome = early_exit_drive(stream, EarlyExitConfig::new(0.0))
        .await
        .expect("zero floor must not trigger on confidence=0.0");
    assert!(outcome.text.contains("confidence"));
}

#[tokio::test]
async fn early_exit_drive_returns_empty_response_when_stream_dries_up() {
    // No Done chunk — the wrapper should classify this as EmptyResponse so
    // the retry layer can re-attempt.
    let stream = boxed(vec![delta(r#"{"confidence": 0.7"#)]);
    match early_exit_drive(stream, EarlyExitConfig::default()).await {
        Err(Error::EmptyResponse) => {}
        other => panic!("expected EmptyResponse, got {other:?}"),
    }
}

#[tokio::test]
async fn early_exit_stream_adapter_delivers_delta_then_early_exit_error() {
    use futures_util::StreamExt;

    let inner = boxed(vec![
        delta(r#"{"confidence": 0.1, "rationale": "..."}"#),
        done(),
    ]);
    let mut stream = EarlyExitStream::new(inner, EarlyExitConfig::new(0.5));

    // First poll: yields the Delta (the one that completed the parse).
    let first = stream.next().await.expect("first item").expect("Ok");
    matches!(first, StreamChunk::Delta { .. });

    // Second poll: yields the armed EarlyExit error.
    let second = stream.next().await.expect("second item");
    match second {
        Err(Error::EarlyExit { confidence, .. }) => {
            assert!((confidence - 0.1).abs() < f32::EPSILON);
        }
        other => panic!("expected EarlyExit on second poll, got {other:?}"),
    }
}

#[tokio::test]
async fn token_saved_metric_is_positive_on_short_partial() {
    let stream = boxed(vec![
        delta(r#"{"confidence": 0.05, "rationale": "x"}"#),
        done(),
    ]);
    let err = early_exit_drive(stream, EarlyExitConfig::new(0.5))
        .await
        .unwrap_err();
    match err {
        Error::EarlyExit {
            output_tokens_saved_estimated,
            ..
        } => assert!(output_tokens_saved_estimated > 0),
        other => panic!("expected EarlyExit, got {other:?}"),
    }
}
