//! Error type for the probe layer.

/// Errors that can occur while probing an inference engine.
///
/// The probe drives an engine over HTTP and reads a streamed
/// response. Each variant marks a distinct failure stage, so a
/// caller can tell a transport failure apart from a malformed
/// stream apart from an engine-side error.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The HTTP request could not be completed: the endpoint was
    /// unreachable, the connection was refused, or it timed out.
    #[error("request to the engine failed: {0}")]
    Transport(#[from] reqwest::Error),

    /// The engine returned a non-success HTTP status. The status
    /// code and any response body are captured for diagnosis.
    #[error("engine returned HTTP {status}: {body}")]
    HttpStatus {
        /// The HTTP status code returned by the engine.
        status: u16,
        /// The response body, truncated if long, for context.
        body: String,
    },

    /// A chunk of the streamed response could not be parsed as the
    /// expected server-sent-events / JSON shape.
    #[error("malformed stream chunk: {reason}")]
    MalformedChunk {
        /// What about the chunk could not be parsed.
        reason: String,
    },

    /// The stream ended before the engine signalled completion.
    /// Any tokens received before the cut are lost with it.
    #[error("stream ended unexpectedly after {tokens_received} token(s)")]
    StreamTruncated {
        /// How many tokens had been received before the stream cut.
        tokens_received: u32,
    },
}
