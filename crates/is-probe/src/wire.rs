//! Wire types for the OpenAI-compatible chat completions API.
//!
//! These types model the JSON exchanged with the engine. This module
//! currently covers the request side — the body the probe sends. The
//! response side (streamed SSE chunks) is added alongside the
//! streaming logic.
//!
//! The types are intentionally minimal: they cover only the fields
//! inferscope needs, not the full OpenAI schema. An engine may accept
//! many more fields, but a profiler only needs to send a
//! well-formed, deterministic request.

use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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

        // Top-level fields match what an OpenAI-compatible engine expects.
        assert_eq!(json["model"], Value::String("llama3".to_string()));
        assert_eq!(json["max_tokens"], Value::from(32));
        assert_eq!(json["temperature"], Value::from(0.0));
        assert_eq!(json["stream"], Value::Bool(true));

        // messages is a one-element array with a user message.
        let messages = json["messages"].as_array().expect("messages must be array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], Value::String("user".to_string()));
        assert_eq!(messages[0]["content"], Value::String("ping".to_string()));
    }
}
