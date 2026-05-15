//! Wire types for the OpenAI-compatible chat completions API.
//!
//! These types model the JSON exchanged with the engine: the request
//! body the probe sends, and the streamed response chunks it
//! receives.
//!
//! The types are intentionally minimal: they cover only the fields
//! inferscope needs, not the full OpenAI schema. An engine may emit
//! many more fields, but a profiler only needs to recognise tokens
//! and the end of the stream.

use serde::{Deserialize, Serialize};

// ----- Request side -----

/// A single chat message in a request.
///
/// inferscope sends exactly one message per probe run — a user
/// message carrying the prompt — but the type models the general
/// shape so the request body stays a faithful OpenAI request.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    /// The role of the message author. inferscope only ever sends
    /// `"user"`, but the field is explicit so the JSON is a valid
    /// chat message.
    pub role: String,

    /// The text content of the message.
    pub content: String,
}

impl ChatMessage {
    /// Creates a user message carrying the given content.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
}

/// The body of a streamed chat completions request.
///
/// Serializes to the JSON an OpenAI-compatible engine expects at
/// `POST /v1/chat/completions`. `stream` is always `true`: the
/// probe measures per-token arrival, which requires the streamed
/// response form.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    /// The model identifier, as the engine expects it.
    pub model: String,

    /// The conversation messages. For a probe run this is a single
    /// user message.
    pub messages: Vec<ChatMessage>,

    /// Maximum number of tokens to generate.
    pub max_tokens: u32,

    /// Sampling temperature. The probe sends `0.0` for deterministic
    /// runs.
    pub temperature: f32,

    /// Always `true`. The probe requires the streamed response form
    /// to measure per-token timing.
    pub stream: bool,
}

impl ChatRequest {
    /// Builds a streamed chat request from the given parameters.
    /// `stream` is set to `true` unconditionally.
    pub fn new(
        model: impl Into<String>,
        prompt: impl Into<String>,
        max_tokens: u32,
        temperature: f32,
    ) -> Self {
        Self {
            model: model.into(),
            messages: vec![ChatMessage::user(prompt)],
            max_tokens,
            temperature,
            stream: true,
        }
    }
}

// ----- Response side -----

/// A single streamed chunk from the engine, after the `data: `
/// prefix has been stripped and the JSON has been parsed.
///
/// An OpenAI-compatible chunk carries one or more choices; in
/// practice for a non-batched request there is exactly one. Each
/// choice has a `delta` (the incremental piece of generated content)
/// and possibly a `finish_reason` (set on the final chunk).
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChunk {
    /// The choices array. For inferscope's single-request usage the
    /// length is always 1, but the type matches the OpenAI shape.
    pub choices: Vec<ChatChoice>,
}

/// One choice within a chunk.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatChoice {
    /// The incremental piece of content for this chunk. May be
    /// empty on the final chunk that carries only a finish reason.
    pub delta: ChatDelta,

    /// Why the generation stopped, if it has. `None` while
    /// generation is still ongoing, `Some` on the terminating
    /// chunk (typical values: `"stop"`, `"length"`).
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// The incremental content portion of a chunk.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatDelta {
    /// The text fragment generated since the previous chunk. May
    /// be `None` on the final chunk that only signals finish.
    #[serde(default)]
    pub content: Option<String>,
}

/// What a single SSE line from the engine represents, after
/// classification.
///
/// The SSE framing of OpenAI-compatible streams uses `data: ` lines.
/// Most carry a JSON chunk; one special line, `data: [DONE]`,
/// terminates the stream. Empty lines and comments are skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseLine {
    /// A normal chunk line carrying parsed JSON content.
    Chunk(String),

    /// The `[DONE]` sentinel that ends the stream.
    Done,

    /// A line that conveys no chunk data (blank line, comment,
    /// or a non-`data:` field). Should be ignored by the consumer.
    Ignore,
}

/// Classifies a single line from an SSE stream.
///
/// The OpenAI streaming format prefixes every data line with
/// `data: `. The special body `[DONE]` (without JSON braces) marks
/// the end of the stream. Anything else is conservatively ignored
/// — including the empty lines that SSE uses as event separators
/// and any comment lines (those starting with `:`).
///
/// Returns the line's classification. JSON parsing of the chunk
/// body is intentionally not done here: this function is just the
/// SSE framing layer.
pub fn classify_sse_line(line: &str) -> SseLine {
    let line = line.trim_end_matches('\r');

    if line.is_empty() || line.starts_with(':') {
        return SseLine::Ignore;
    }

    if let Some(payload) = line
        .strip_prefix("data: ")
        .or_else(|| line.strip_prefix("data:"))
    {
        let payload = payload.trim_start();
        if payload == "[DONE]" {
            SseLine::Done
        } else {
            SseLine::Chunk(payload.to_string())
        }
    } else {
        SseLine::Ignore
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // ----- Request tests -----

    #[test]
    fn chat_message_user_sets_the_role() {
        let m = ChatMessage::user("hello");
        assert_eq!(m.role, "user");
        assert_eq!(m.content, "hello");
    }

    #[test]
    fn chat_request_new_forces_stream_true() {
        let req = ChatRequest::new("llama3", "hi", 64, 0.0);
        assert!(
            req.stream,
            "the probe must request the streamed response form"
        );
    }

    #[test]
    fn chat_request_serializes_to_expected_json_shape() {
        let req = ChatRequest::new("llama3", "ping", 32, 0.0);
        let json = serde_json::to_value(&req).expect("ChatRequest should serialize");

        assert_eq!(json["model"], Value::String("llama3".to_string()));
        assert_eq!(json["max_tokens"], Value::from(32));
        assert_eq!(json["temperature"], Value::from(0.0));
        assert_eq!(json["stream"], Value::Bool(true));

        let messages = json["messages"].as_array().expect("messages must be array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], Value::String("user".to_string()));
        assert_eq!(messages[0]["content"], Value::String("ping".to_string()));
    }

    // ----- Response classification tests -----

    #[test]
    fn classify_recognises_a_data_line_with_json() {
        let got = classify_sse_line(r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#);
        match got {
            SseLine::Chunk(payload) => {
                assert!(payload.starts_with('{'));
                assert!(payload.contains(r#""content":"hi""#));
            }
            other => panic!("expected Chunk, got {other:?}"),
        }
    }

    #[test]
    fn classify_recognises_the_done_sentinel() {
        assert_eq!(classify_sse_line("data: [DONE]"), SseLine::Done);
    }

    #[test]
    fn classify_tolerates_data_without_space() {
        // Some servers emit `data:{...}` with no space. Accept both.
        let got = classify_sse_line(r#"data:{"choices":[{"delta":{"content":"x"}}]}"#);
        assert!(matches!(got, SseLine::Chunk(_)));
    }

    #[test]
    fn classify_strips_trailing_carriage_return() {
        // SSE over HTTP uses CRLF line endings.
        assert_eq!(classify_sse_line("data: [DONE]\r"), SseLine::Done);
    }

    #[test]
    fn classify_ignores_blank_lines() {
        assert_eq!(classify_sse_line(""), SseLine::Ignore);
    }

    #[test]
    fn classify_ignores_comment_lines() {
        assert_eq!(classify_sse_line(": this is a comment"), SseLine::Ignore);
    }

    #[test]
    fn classify_ignores_unknown_fields() {
        assert_eq!(classify_sse_line("event: ping"), SseLine::Ignore);
        assert_eq!(classify_sse_line("id: 42"), SseLine::Ignore);
    }

    // ----- Response JSON deserialization tests -----

    #[test]
    fn chunk_with_content_deserializes() {
        let json = r#"{"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk: ChatChunk = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(chunk.choices.len(), 1);
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
        assert_eq!(chunk.choices[0].finish_reason, None);
    }

    #[test]
    fn final_chunk_with_finish_reason_deserializes() {
        let json = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let chunk: ChatChunk = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(chunk.choices[0].delta.content, None);
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn chunk_ignores_unknown_top_level_fields() {
        // Real engines emit id, object, created, model, system_fingerprint
        // etc. We don't model them; they must be silently accepted.
        let json = r#"{
            "id": "chatcmpl-abc",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "llama3",
            "choices": [{"delta": {"content": "hi"}, "finish_reason": null}]
        }"#;
        let chunk: ChatChunk = serde_json::from_str(json).expect("should deserialize");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
    }
}
