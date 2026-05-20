//! Anthropic SSE (server-sent events) parser for the Messages API
//! streaming mode.
//!
//! Wire format (excerpt from the Messages streaming docs):
//!
//! ```text
//! event: message_start
//! data: {"type":"message_start","message":{...,"usage":{"input_tokens":N,...}}}
//!
//! event: content_block_delta
//! data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"…"}}
//!
//! event: content_block_start | content_block_stop  (ignored — text deltas suffice)
//!
//! event: message_delta
//! data: {"type":"message_delta","usage":{"output_tokens":N}}
//!
//! event: message_stop
//! data: {"type":"message_stop"}
//!
//! event: ping     (heartbeat; ignored)
//! event: error    (terminal — map to Error::Status)
//! ```
//!
//! Each event is separated by a blank line. We only care about the `data:`
//! field — the `event:` field is informational. The parser:
//!
//! 1. Accumulates a chunk of bytes into a UTF-8 string buffer.
//! 2. Splits on `\n\n` (event boundaries).
//! 3. For each event, extracts the `data: …` payload, JSON-decodes it,
//!    dispatches by `type`.
//! 4. Text deltas become [`StreamChunk::Delta`].
//! 5. `message_start.usage.input_tokens` (plus cache fields) seed the
//!    running usage; `message_delta.usage.output_tokens` updates it;
//!    `message_stop` emits [`StreamChunk::Done`].
//! 6. `error` events become a `StreamChunk` carrying an [`Error::Status`].

use std::time::Instant;

use futures_util::Stream;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::streaming::StreamChunk;
use crate::types::{Cost, Model, Usage};

/// Parse an Anthropic SSE byte stream into a stream of [`StreamChunk`]s.
///
/// `started` is the wall-clock instant the HTTP request was issued, used
/// to populate the latency field of the terminal [`StreamChunk::Done`].
/// `model` is the [`Model`] passed to the call and is used for cost
/// computation in the terminal [`StreamChunk::Done`].
pub(crate) fn parse_anthropic_sse<S>(
    bytes: S,
    started: Instant,
    model: Model,
) -> impl Stream<Item = Result<StreamChunk>> + Send
where
    S: Stream<Item = Result<bytes::Bytes>> + Send + 'static,
{
    use futures_util::stream::unfold;

    enum State<S> {
        /// Still pulling from the source stream; `buffer` holds bytes that
        /// have not yet formed a complete event.
        Reading {
            source: std::pin::Pin<Box<S>>,
            buffer: String,
            running_usage: Usage,
            model_used: String,
            model: Model,
            started: Instant,
            done_emitted: bool,
        },
        /// Terminal — source drained and Done already emitted.
        Done,
    }

    let init = State::Reading {
        source: Box::pin(bytes),
        buffer: String::new(),
        running_usage: Usage::default(),
        model_used: String::new(),
        model,
        started,
        done_emitted: false,
    };

    unfold(init, |state| async move {
        let State::Reading {
            mut source,
            mut buffer,
            mut running_usage,
            mut model_used,
            model,
            started,
            mut done_emitted,
        } = state
        else {
            return None;
        };

        loop {
            // Drain any complete events already in the buffer.
            while let Some(boundary) = buffer.find("\n\n") {
                let raw_event = buffer[..boundary].to_string();
                buffer.drain(..boundary + 2);

                let outcome = handle_event(
                    &raw_event,
                    &mut running_usage,
                    &mut model_used,
                    &model,
                    started,
                );
                match outcome {
                    EventOutcome::Skip => continue,
                    EventOutcome::Emit(chunk) => {
                        let is_done = matches!(chunk, StreamChunk::Done { .. });
                        if is_done {
                            done_emitted = true;
                        }
                        let next_state = if is_done {
                            State::Done
                        } else {
                            State::Reading {
                                source,
                                buffer,
                                running_usage,
                                model_used,
                                model,
                                started,
                                done_emitted,
                            }
                        };
                        return Some((Ok(chunk), next_state));
                    }
                    EventOutcome::Error(e) => {
                        return Some((Err(e), State::Done));
                    }
                }
            }

            // No complete event yet — pull more bytes.
            use futures_util::StreamExt;
            match source.next().await {
                Some(Ok(chunk)) => {
                    // Tolerate non-UTF-8 by using lossy conversion; SSE
                    // payloads from Anthropic are UTF-8 JSON.
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                }
                Some(Err(e)) => return Some((Err(e), State::Done)),
                None => {
                    // Source exhausted. If we haven't emitted Done, the
                    // stream ended unexpectedly — surface as error rather
                    // than silently dropping.
                    if !done_emitted {
                        return Some((
                            Err(Error::Decode(
                                "stream ended before message_stop".to_string(),
                            )),
                            State::Done,
                        ));
                    }
                    return None;
                }
            }
        }
    })
}

enum EventOutcome {
    Skip,
    Emit(StreamChunk),
    Error(Error),
}

fn handle_event(
    raw_event: &str,
    running_usage: &mut Usage,
    model_used: &mut String,
    model: &Model,
    started: Instant,
) -> EventOutcome {
    // Extract the `data:` payload. SSE allows multiple `data:` lines per
    // event; concatenate them with `\n` per spec. Most Anthropic events
    // have one line.
    let mut data = String::new();
    let mut event_name = "";
    for line in raw_event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        } else if let Some(rest) = line.strip_prefix("event:") {
            event_name = rest.trim();
        }
        // Other fields (`id:`, `retry:`) are ignored.
    }
    if data.is_empty() {
        return EventOutcome::Skip;
    }

    let parsed: SseFrame = match serde_json::from_str(&data) {
        Ok(p) => p,
        Err(e) => {
            return EventOutcome::Error(Error::Decode(format!(
                "sse data on event `{event_name}`: {e}"
            )))
        }
    };

    match parsed {
        SseFrame::MessageStart { message } => {
            *model_used = format!("anthropic/{}", message.model);
            running_usage.input_tokens_total += message.usage.input_tokens
                + message.usage.cache_read_input_tokens.unwrap_or(0)
                + message.usage.cache_creation_input_tokens.unwrap_or(0);
            running_usage.input_tokens_cached += message.usage.cache_read_input_tokens.unwrap_or(0);
            running_usage.input_tokens_cache_create +=
                message.usage.cache_creation_input_tokens.unwrap_or(0);
            running_usage.output_tokens += message.usage.output_tokens;
            EventOutcome::Skip
        }
        SseFrame::ContentBlockDelta { delta } => match delta {
            BlockDelta::TextDelta { text } => EventOutcome::Emit(StreamChunk::Delta { text }),
            BlockDelta::Other => EventOutcome::Skip,
        },
        SseFrame::ContentBlockStart { content_block } => {
            // Forward-compat marker for tool-use blocks. Text starts are
            // a no-op (deltas carry the content).
            match content_block {
                ContentBlockStart::ToolUse => EventOutcome::Emit(StreamChunk::ToolUseMarker),
                ContentBlockStart::Other => EventOutcome::Skip,
            }
        }
        SseFrame::MessageDelta { usage } => {
            // `message_delta.usage.output_tokens` is the running total per
            // Anthropic; replace rather than add.
            running_usage.output_tokens = usage.output_tokens;
            EventOutcome::Skip
        }
        SseFrame::MessageStop => {
            let cost = Cost::from_usage(running_usage, model);
            EventOutcome::Emit(StreamChunk::Done {
                usage: *running_usage,
                cost,
                model_used: model_used.clone(),
                latency_ms: started.elapsed().as_millis() as u64,
            })
        }
        SseFrame::ErrorFrame { error } => EventOutcome::Error(Error::Status {
            status: 0,
            message: error.message,
        }),
        SseFrame::Ping | SseFrame::ContentBlockStop | SseFrame::Other => EventOutcome::Skip,
    }
}

// ─── wire shape ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseFrame {
    MessageStart {
        message: MessageStartBody,
    },
    ContentBlockStart {
        content_block: ContentBlockStart,
    },
    ContentBlockDelta {
        delta: BlockDelta,
    },
    ContentBlockStop,
    MessageDelta {
        usage: DeltaUsage,
    },
    MessageStop,
    Ping,
    #[serde(rename = "error")]
    ErrorFrame {
        error: ApiError,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MessageStartBody {
    model: String,
    usage: StartUsage,
}

#[derive(Deserialize)]
struct StartUsage {
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct DeltaUsage {
    output_tokens: u32,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDelta {
    TextDelta {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockStart {
    ToolUse,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct ApiError {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, StreamExt};

    /// Drive the parser with a single big byte chunk holding the entire
    /// canned SSE response.
    async fn run_parser(payload: &'static str) -> Vec<Result<StreamChunk>> {
        let source = stream::iter(vec![Ok(bytes::Bytes::from(payload))]);
        let parsed = parse_anthropic_sse(
            source,
            Instant::now(),
            Model::anthropic("claude-3-5-haiku-20241022"),
        );
        let mut out = Vec::new();
        let mut s = std::pin::pin!(parsed);
        while let Some(item) = s.next().await {
            out.push(item);
        }
        out
    }

    #[tokio::test]
    async fn parses_normal_stream() {
        let payload = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"model\":\"claude-3-5-haiku-20241022\",\"usage\":{\"input_tokens\":10,\"output_tokens\":0,\"cache_read_input_tokens\":4}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";

        let chunks = run_parser(payload).await;
        assert_eq!(
            chunks.len(),
            3,
            "expected 2 deltas + 1 done, got {chunks:?}"
        );

        match &chunks[0] {
            Ok(StreamChunk::Delta { text }) => assert_eq!(text, "hello"),
            other => panic!("expected Delta(hello), got {other:?}"),
        }
        match &chunks[1] {
            Ok(StreamChunk::Delta { text }) => assert_eq!(text, " world"),
            other => panic!("expected Delta( world), got {other:?}"),
        }
        match &chunks[2] {
            Ok(StreamChunk::Done {
                usage, model_used, ..
            }) => {
                assert_eq!(model_used, "anthropic/claude-3-5-haiku-20241022");
                // 10 (input) + 4 (cache_read) = 14 total; cached = 4
                assert_eq!(usage.input_tokens_total, 14);
                assert_eq!(usage.input_tokens_cached, 4);
                assert_eq!(usage.output_tokens, 7);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parser_tolerates_split_chunks() {
        // Same as above but split in the middle of an event.
        let halves: Vec<&'static str> = vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: content_block_delta\nda",
            "ta: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"split-ok\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ];
        let owned: Vec<Result<bytes::Bytes>> = halves
            .into_iter()
            .map(|s| Ok(bytes::Bytes::from(s)))
            .collect();
        let source = stream::iter(owned);
        let parsed = parse_anthropic_sse(
            source,
            Instant::now(),
            Model::anthropic("claude-3-5-haiku-20241022"),
        );
        let mut s = std::pin::pin!(parsed);
        let mut deltas = Vec::new();
        let mut done = false;
        while let Some(item) = s.next().await {
            match item {
                Ok(StreamChunk::Delta { text }) => deltas.push(text),
                Ok(StreamChunk::Done { .. }) => done = true,
                Ok(StreamChunk::ToolUseMarker) => {}
                Err(e) => panic!("parser error: {e:?}"),
            }
        }
        assert_eq!(deltas, vec!["split-ok"]);
        assert!(done);
    }

    #[tokio::test]
    async fn parser_surfaces_error_frame() {
        let payload = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"too busy\"}}\n\n";
        let chunks = run_parser(payload).await;
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            Err(Error::Status { message, .. }) => assert_eq!(message, "too busy"),
            other => panic!("expected Status error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn parser_errors_on_truncated_stream() {
        // Missing message_stop.
        let payload = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n";
        let chunks = run_parser(payload).await;
        // 1 delta + 1 truncation error.
        assert_eq!(chunks.len(), 2);
        assert!(matches!(chunks[0], Ok(StreamChunk::Delta { .. })));
        assert!(matches!(chunks[1], Err(Error::Decode(_))));
    }

    #[tokio::test]
    async fn parser_emits_tool_use_marker_on_tool_use_block_start() {
        let payload = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"calc\",\"input\":{}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let chunks = run_parser(payload).await;
        assert_eq!(chunks.len(), 2);
        assert!(matches!(chunks[0], Ok(StreamChunk::ToolUseMarker)));
        assert!(matches!(chunks[1], Ok(StreamChunk::Done { .. })));
    }

    #[tokio::test]
    async fn parser_skips_ping_events() {
        let payload = "event: ping\ndata: {\"type\":\"ping\"}\n\nevent: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"x\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let chunks = run_parser(payload).await;
        // Just one Done, no errors.
        assert_eq!(chunks.len(), 1);
        assert!(matches!(chunks[0], Ok(StreamChunk::Done { .. })));
    }
}
