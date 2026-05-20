//! Streaming early-exit wrapper for [`crate::LlmProvider::complete_streamed`].
//!
//! Wraps a streamed completion in a state machine that scans the incoming
//! [`crate::StreamChunk::Delta`] payloads for the *first complete*
//! `"confidence": N` field. When that value is below the configured floor,
//! the wrapper drops the underlying stream (which cancels the upstream HTTP
//! request — `reqwest` aborts the connection on stream drop) and surfaces
//! [`crate::Error::EarlyExit`] with the partial text and a rough estimate
//! of output tokens saved.
//!
//! # Schema discipline
//!
//! The wrapper only works if the agent's structured-output schema puts
//! `confidence` *first*, before any expensive payload fields. The prompt
//! must instruct the model to emit fields in this order. See
//! `docs/design/03-architecture.md` §Streaming structured output with
//! early-exit.
//!
//! # Composition
//!
//! Stack inside the resilience layers — early-exit operates on the stream
//! returned by `complete_streamed`, so it sits closest to the inner
//! provider:
//!
//! ```text
//! CircuitBreaker(Retry(Timeout(EarlyExit(AnthropicProvider))))
//! ```
//!
//! Witness opts out (`early_exit_enabled = false`) since it has no
//! quality-vs-cost tradeoff. Conversational agents (Pair-Thinking, Socratic
//! Prober) use streaming for UI but don't apply early-exit — they emit
//! free-form text, not structured JSON.
//!
//! # Incremental parsing
//!
//! The parser is **scan-based, not a full JSON parser**. It tracks bracket
//! and quote depth so it can ignore `"confidence":` strings that appear
//! inside other string values, then parses the first numeric run that
//! follows the unescaped key. This avoids pulling in `serde_json::Deserializer`
//! reentrancy issues and stays robust against the leading whitespace,
//! optional `{` delimiters, and unfinished JSON that arrive mid-stream.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::stream::Stream;

use crate::error::{Error, Result};
use crate::streaming::{StreamChunk, StreamedCompletion};

// ---------------------------------------------------------------------------
// Confidence parser
// ---------------------------------------------------------------------------

/// Incremental scanner that finds the first complete `"confidence": N` in a
/// growing JSON document.
///
/// Feed every text fragment via [`Self::feed`]; once a value is found,
/// [`Self::feed`] returns `Some(value)` on the call that completes the
/// numeric run. After that the scanner is *done* — subsequent `feed` calls
/// return `None` and don't mutate state.
///
/// The scanner does *not* validate the surrounding JSON; it deliberately
/// ignores nesting and types beyond what's needed to recognize the
/// top-level `"confidence":` key.
#[derive(Debug, Default)]
pub(crate) struct ConfidenceScanner {
    /// All text seen so far (only what's needed to keep scanning forward).
    buffer: String,
    /// Byte offset into `buffer` where scanning will resume.
    cursor: usize,
    /// Once a confidence value has been emitted, the scanner stops.
    done: bool,
}

impl ConfidenceScanner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns `true` once the scanner has emitted a value (or determined
    /// there is no possible match left).
    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Append a fragment and try to extract the first complete
    /// `"confidence": N` value. Returns `Some(value)` exactly once, on the
    /// call that completes the numeric run.
    pub(crate) fn feed(&mut self, fragment: &str) -> Option<f32> {
        if self.done {
            return None;
        }
        self.buffer.push_str(fragment);
        let value = scan_confidence(&self.buffer[self.cursor..]);
        match value {
            ScanOutcome::Found(v, end_offset) => {
                self.cursor += end_offset;
                self.done = true;
                Some(v)
            }
            ScanOutcome::NeedMore => None,
            ScanOutcome::Impossible => {
                // The buffer contains a closing `}` after a top-level
                // payload that didn't include `confidence`. There is no
                // point scanning further chunks.
                self.done = true;
                None
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum ScanOutcome {
    /// A confidence value was extracted; the second element is the byte
    /// offset in `slice` *just past* the numeric run.
    Found(f32, usize),
    /// Need more text to decide.
    NeedMore,
    /// The buffer demonstrably contains no `"confidence"` key at the top
    /// level (e.g. the top-level object has closed without one).
    Impossible,
}

/// Scan `slice` (assumed to be the as-yet-unscanned suffix of a JSON
/// document being streamed in) for a top-level `"confidence": N` pair.
///
/// Tracks bracket depth so a nested `"confidence"` (e.g. inside a string
/// value or an embedded object) is ignored.
fn scan_confidence(slice: &str) -> ScanOutcome {
    const KEY: &str = "\"confidence\"";

    let bytes = slice.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];

        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                // We've just exited a string. Before exiting, check if the
                // string we just exited is the literal "confidence" *at*
                // top-level depth (i.e. depth == 1 since the opening `{`
                // has incremented depth before any keys). The byte at the
                // start of this string was at `string_start`; we don't
                // track that explicitly. Instead we use a forward-match
                // approach below: when we encounter `"confidence"` while
                // *not yet in a string* and depth == 1, we trigger.
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                depth -= 1;
                i += 1;
                if depth <= 0 {
                    // Top-level object closed without a confidence field.
                    return ScanOutcome::Impossible;
                }
            }
            b'"' => {
                // Check for a top-level `"confidence"` key. Only meaningful
                // at depth == 1 (inside the outermost object).
                if depth == 1 && slice[i..].starts_with(KEY) {
                    let after_key = i + KEY.len();
                    return parse_value_after_key(slice, after_key);
                }
                in_string = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    // Reached end of buffer without closing the top-level object and
    // without finding the key — keep waiting for more chunks.
    ScanOutcome::NeedMore
}

/// Helper: starting at byte `idx` in `slice` (just past `"confidence"`),
/// look for `:` and then the numeric run. Returns [`ScanOutcome::Found`]
/// only when the number is *complete* — i.e. the byte after the digits is
/// a JSON separator (`,`, `}`, whitespace) so we know no more digits will
/// follow. Otherwise returns [`ScanOutcome::NeedMore`].
fn parse_value_after_key(slice: &str, idx: usize) -> ScanOutcome {
    let bytes = slice.as_bytes();
    let mut i = idx;

    // Skip whitespace, then expect `:`.
    while i < bytes.len() && is_json_ws(bytes[i]) {
        i += 1;
    }
    if i >= bytes.len() {
        return ScanOutcome::NeedMore;
    }
    if bytes[i] != b':' {
        // Malformed (or scanner saw a string ending in "confidence"
        // without a colon). Treat as no-match for now.
        return ScanOutcome::Impossible;
    }
    i += 1;
    while i < bytes.len() && is_json_ws(bytes[i]) {
        i += 1;
    }
    if i >= bytes.len() {
        return ScanOutcome::NeedMore;
    }

    // Numeric run: optional `-`, digits, optional `.`, digits, optional
    // exponent. We greedily consume valid bytes and stop at the first
    // delimiter — but we have to wait for the delimiter to know the value
    // is complete (otherwise `0.2` may be only `0.2` so far when the model
    // has more digits to emit).
    let num_start = i;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }

    if i >= bytes.len() {
        // The number runs to end-of-buffer; we don't yet know whether more
        // digits are coming. Wait for the next chunk.
        return ScanOutcome::NeedMore;
    }
    // Next byte must be a JSON terminator. Otherwise the buffer is
    // malformed (or the number isn't a number — e.g. `null`/`true`).
    let next = bytes[i];
    if !(next == b',' || next == b'}' || next == b']' || is_json_ws(next)) {
        return ScanOutcome::Impossible;
    }

    let num_str = &slice[num_start..i];
    match num_str.parse::<f32>() {
        Ok(v) => ScanOutcome::Found(v, i),
        Err(_) => ScanOutcome::Impossible,
    }
}

fn is_json_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

// ---------------------------------------------------------------------------
// Early-exit stream wrapper
// ---------------------------------------------------------------------------

/// Per-call early-exit policy. Construct with [`EarlyExitConfig::new`].
#[derive(Debug, Clone, Copy)]
pub struct EarlyExitConfig {
    /// Cancel the stream when the parsed `confidence` field is *below* this
    /// value. Default per the architecture doc: `0.3`.
    pub floor: f32,
    /// Heuristic for the "tokens saved" metric: assume the model would
    /// have emitted up to this many output tokens had we let it finish.
    /// Used to compute `output_tokens_saved_estimated`.
    pub typical_max_output_tokens: u32,
}

impl Default for EarlyExitConfig {
    fn default() -> Self {
        Self {
            floor: 0.3,
            typical_max_output_tokens: 512,
        }
    }
}

impl EarlyExitConfig {
    /// Build a config with an explicit floor. `floor` is clamped to
    /// `[0.0, 1.0]`.
    pub fn new(floor: f32) -> Self {
        Self {
            floor: floor.clamp(0.0, 1.0),
            ..Self::default()
        }
    }
}

/// Outcome of a streamed call once the wrapper has run to completion.
///
/// This is what the wrapper's standalone entry point ([`early_exit_drive`])
/// returns. The drive function consumes the stream and accumulates the
/// text, returning the final completion shape (text + usage) or
/// [`Error::EarlyExit`] if the floor was crossed.
#[derive(Debug, Clone)]
pub struct StreamedOutcome {
    /// Concatenated text from every `Delta` chunk.
    pub text: String,
    /// Final usage from the terminal `Done` chunk.
    pub usage: crate::Usage,
    /// Final cost from the terminal `Done` chunk.
    pub cost: crate::Cost,
    /// Model identifier from the terminal `Done` chunk.
    pub model_used: String,
    /// Wall-clock latency from the terminal `Done` chunk.
    pub latency_ms: u64,
}

/// Drive a streamed completion under the early-exit policy.
///
/// Consumes the [`StreamedCompletion`] to completion (or to early-exit),
/// accumulating the text. Returns the assembled outcome or
/// [`Error::EarlyExit`] when the floor is crossed.
///
/// This is the function callers use; they don't need to construct the
/// state machine directly.
pub async fn early_exit_drive(
    mut stream: StreamedCompletion,
    config: EarlyExitConfig,
) -> Result<StreamedOutcome> {
    use futures_util::StreamExt;

    let mut scanner = ConfidenceScanner::new();
    let mut text = String::new();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        match chunk {
            StreamChunk::Delta { text: delta } => {
                text.push_str(&delta);
                if !scanner.done() {
                    if let Some(confidence) = scanner.feed(&delta) {
                        if confidence < config.floor {
                            // Drop the stream to cancel the upstream HTTP
                            // request before allocating the StreamedOutcome.
                            drop(stream);
                            let saved =
                                estimate_tokens_saved(&text, config.typical_max_output_tokens);
                            tracing::info!(
                                confidence,
                                floor = config.floor,
                                partial_chars = text.len(),
                                output_tokens_saved_estimated = saved,
                                "engram-llm early-exit: stream cancelled"
                            );
                            return Err(Error::EarlyExit {
                                confidence,
                                floor: config.floor,
                                partial: text,
                                output_tokens_saved_estimated: saved,
                            });
                        }
                    }
                }
            }
            StreamChunk::ToolUseMarker => {}
            StreamChunk::Done {
                usage,
                cost,
                model_used,
                latency_ms,
            } => {
                return Ok(StreamedOutcome {
                    text,
                    usage,
                    cost,
                    model_used,
                    latency_ms,
                });
            }
        }
    }

    // Stream ended without a Done chunk — treat as EmptyResponse so
    // upstream classification routes it to retry.
    Err(Error::EmptyResponse)
}

/// Roughly: assume the model would have emitted `typical_max` output
/// tokens; subtract the tokens we estimate it *did* emit (1 token ≈ 4
/// characters). Bottoms out at zero.
fn estimate_tokens_saved(partial: &str, typical_max: u32) -> u32 {
    let emitted = (partial.len() / 4) as u32;
    typical_max.saturating_sub(emitted)
}

// ---------------------------------------------------------------------------
// Stream adapter (advanced — wrap-and-poll, for callers that need a stream
// rather than an awaited outcome)
// ---------------------------------------------------------------------------

/// Stream adapter that filters [`StreamChunk::Delta`] events through the
/// early-exit scanner. Once the floor is crossed, the next poll returns
/// [`Error::EarlyExit`] and the wrapped stream is dropped on the
/// subsequent poll.
///
/// Most callers want [`early_exit_drive`] instead — it consumes the
/// stream and returns the final outcome. This adapter is for the rare
/// case where the caller wants to keep streaming chunks for UI display.
pub struct EarlyExitStream {
    inner: Option<StreamedCompletion>,
    scanner: ConfidenceScanner,
    accumulated: String,
    config: EarlyExitConfig,
    armed_error: Option<Error>,
}

impl EarlyExitStream {
    /// Wrap `inner` with the configured early-exit policy.
    pub fn new(inner: StreamedCompletion, config: EarlyExitConfig) -> Self {
        Self {
            inner: Some(inner),
            scanner: ConfidenceScanner::new(),
            accumulated: String::new(),
            config,
            armed_error: None,
        }
    }
}

impl Stream for EarlyExitStream {
    type Item = Result<StreamChunk>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // If we armed an EarlyExit on a previous poll, deliver it now and
        // drop the inner stream to cancel the upstream request.
        if let Some(err) = self.armed_error.take() {
            self.inner = None;
            return Poll::Ready(Some(Err(err)));
        }

        let Some(inner) = self.inner.as_mut() else {
            return Poll::Ready(None);
        };

        match Pin::new(inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Some(Ok(chunk))) => {
                if let StreamChunk::Delta { text } = &chunk {
                    self.accumulated.push_str(text);
                    if !self.scanner.done() {
                        if let Some(confidence) = self.scanner.feed(text) {
                            if confidence < self.config.floor {
                                let saved = estimate_tokens_saved(
                                    &self.accumulated,
                                    self.config.typical_max_output_tokens,
                                );
                                tracing::info!(
                                    confidence,
                                    floor = self.config.floor,
                                    partial_chars = self.accumulated.len(),
                                    output_tokens_saved_estimated = saved,
                                    "engram-llm early-exit: stream cancelled (adapter)"
                                );
                                self.armed_error = Some(Error::EarlyExit {
                                    confidence,
                                    floor: self.config.floor,
                                    partial: std::mem::take(&mut self.accumulated),
                                    output_tokens_saved_estimated: saved,
                                });
                                // Deliver this last delta first; the next
                                // poll will return the EarlyExit.
                            }
                        }
                    }
                }
                Poll::Ready(Some(Ok(chunk)))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_complete_low_confidence_in_one_feed() {
        let mut s = ConfidenceScanner::new();
        let v = s
            .feed(r#"{"confidence": 0.2, "rationale": "..."}"#)
            .unwrap();
        assert!((v - 0.2).abs() < f32::EPSILON);
        assert!(s.done());
    }

    #[test]
    fn scans_across_chunks_with_split_in_number() {
        let mut s = ConfidenceScanner::new();
        // Feed in fragments that split the number mid-digit.
        assert!(s.feed(r#"{"confidence": 0."#).is_none());
        assert!(s.feed("8").is_none()); // could still be 0.85 or 0.8
        let v = s.feed(",").unwrap();
        assert!((v - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn ignores_confidence_inside_a_string_value() {
        let mut s = ConfidenceScanner::new();
        let payload = r#"{"rationale": "the model said \"confidence\": false in its body", "confidence": 0.42}"#;
        let v = s.feed(payload).unwrap();
        assert!((v - 0.42).abs() < f32::EPSILON);
    }

    #[test]
    fn impossible_when_top_level_closes_without_confidence() {
        let mut s = ConfidenceScanner::new();
        assert!(s.feed(r#"{"rationale": "abc"}"#).is_none());
        assert!(s.done(), "should mark done as Impossible");
    }

    #[test]
    fn does_not_emit_until_number_is_complete() {
        let mut s = ConfidenceScanner::new();
        // Without a delimiter the parser must not commit — the very next
        // chunk could append another digit.
        assert!(
            s.feed(r#"{"confidence": 0.2"#).is_none(),
            "trailing digit could continue; must not emit yet"
        );
        assert!(
            s.feed("3").is_none(),
            "still no delimiter; the value could be 0.234..."
        );
        // The terminator commits the value.
        let v = s.feed(",").expect("comma terminates the number");
        assert!((v - 0.23).abs() < f32::EPSILON);
    }

    #[test]
    fn config_floor_clamps_to_unit_interval() {
        let c = EarlyExitConfig::new(1.5);
        assert!((c.floor - 1.0).abs() < f32::EPSILON);
        let c = EarlyExitConfig::new(-0.1);
        assert!(c.floor.abs() < f32::EPSILON);
    }

    #[test]
    fn estimate_tokens_saved_returns_positive_when_under_typical() {
        let saved = estimate_tokens_saved("a".repeat(40).as_str(), 512);
        // 40 chars ≈ 10 tokens emitted, 512 - 10 = 502 saved.
        assert_eq!(saved, 502);
    }

    #[test]
    fn estimate_tokens_saved_bottoms_at_zero() {
        let saved = estimate_tokens_saved("a".repeat(10_000).as_str(), 512);
        assert_eq!(saved, 0);
    }
}
