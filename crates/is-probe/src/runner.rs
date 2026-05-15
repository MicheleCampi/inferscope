//! The streaming probe runner.
//!
//! [`run`] is the function that ties the wire types, the line
//! decoder, and the token extractor to actual network I/O. It opens
//! a streamed chat completions request, captures one reference
//! instant at the moment the request is sent, and records each
//! arriving token's offset from that instant. It returns a
//! [`RequestTiming`] holding the raw signal.
//!
//! This is the only module in the crate that performs I/O.

use std::time::Instant;

use is_core::{RequestTiming, TokenArrival};

use crate::config::ProbeConfig;
use crate::decoder::LineDecoder;
use crate::error::ProbeError;
use crate::extract::{extract_token, ChunkOutcome};
use crate::wire::{classify_sse_line, ChatChunk, ChatRequest, SseLine};

/// Runs one probe: sends a streamed chat completions request and
/// captures the timing of every arriving token.
///
/// Returns the [`RequestTiming`] describing the run. The reference
/// instant for `elapsed_ns` values is the moment immediately before
/// the HTTP request is dispatched. `total_ns` is measured up to the
/// point the stream terminates (either via the engine's `[DONE]`
/// sentinel, via a chunk with a `finish_reason`, or via the
/// underlying byte stream ending).
///
/// Returns [`ProbeError::Transport`] if the connection itself fails,
/// [`ProbeError::HttpStatus`] if the engine responds with a non-2xx
/// status, [`ProbeError::MalformedChunk`] if an SSE data line fails
/// to parse as a chat chunk, and [`ProbeError::StreamTruncated`] if
/// the byte stream ends before the engine signals completion.
pub async fn run(config: &ProbeConfig) -> Result<RequestTiming, ProbeError> {
    let body = ChatRequest::new(
        &config.model,
        &config.prompt,
        config.max_tokens,
        config.temperature,
    );

    let client = reqwest::Client::new();

    // The reference instant: every TokenArrival.elapsed_ns is the
    // difference between an arrival moment and this point. Captured
    // immediately before the request leaves so it includes the full
    // round trip.
    let start = Instant::now();

    let response = client
        .post(config.completions_url())
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        // Read the body for diagnostics, capped to keep error
        // messages reasonable. A 4xx/5xx body is typically a short
        // JSON error from the engine.
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| String::from("<failed to read error body>"));
        let truncated = if body_text.len() > 512 {
            format!("{}…", &body_text[..512])
        } else {
            body_text
        };
        return Err(ProbeError::HttpStatus {
            status: status.as_u16(),
            body: truncated,
        });
    }

    let mut decoder = LineDecoder::new();
    let mut tokens: Vec<TokenArrival> = Vec::new();
    let mut finished = false;

    // reqwest's chunk() pulls the next chunk of body bytes as they
    // arrive; the byte boundaries are dictated by the transport,
    // not by line boundaries.
    let mut response = response;
    while let Some(chunk) = response.chunk().await? {
        decoder.push(&chunk);

        loop {
            let line = match decoder.next_line() {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(e) => {
                    return Err(ProbeError::MalformedChunk {
                        reason: format!("invalid UTF-8 in stream: {e}"),
                    });
                }
            };

            match classify_sse_line(&line) {
                SseLine::Ignore => continue,
                SseLine::Done => {
                    finished = true;
                    break;
                }
                SseLine::Chunk(payload) => {
                    let chat_chunk: ChatChunk =
                        serde_json::from_str(&payload).map_err(|e| ProbeError::MalformedChunk {
                            reason: format!("JSON parse error: {e}"),
                        })?;

                    match extract_token(&chat_chunk) {
                        ChunkOutcome::Token(text) => {
                            let elapsed_ns = start.elapsed().as_nanos() as u64;
                            let index = tokens.len() as u32;
                            tokens.push(TokenArrival::new(index, elapsed_ns));
                            // The text content itself is currently
                            // discarded: only its arrival timing is
                            // recorded. Capturing the text is a
                            // future addition; the timing is the
                            // primary signal.
                            let _ = text;
                        }
                        ChunkOutcome::Finished(_reason) => {
                            finished = true;
                            break;
                        }
                        ChunkOutcome::Empty => continue,
                    }
                }
            }
        }

        if finished {
            break;
        }
    }

    // If the inner loop broke on Done or Finished, finished is true.
    // If the byte stream simply ran out, we may still have a
    // trailing partial line in the decoder. Flush it.
    if !finished {
        if let Some(line) = decoder.finish().map_err(|e| ProbeError::MalformedChunk {
            reason: format!("invalid UTF-8 in trailing line: {e}"),
        })? {
            // Treat the trailing line uniformly with the others.
            match classify_sse_line(&line) {
                SseLine::Done => finished = true,
                SseLine::Chunk(payload) => {
                    if let Ok(chunk) = serde_json::from_str::<ChatChunk>(&payload) {
                        match extract_token(&chunk) {
                            ChunkOutcome::Token(_) => {
                                let elapsed_ns = start.elapsed().as_nanos() as u64;
                                let index = tokens.len() as u32;
                                tokens.push(TokenArrival::new(index, elapsed_ns));
                            }
                            ChunkOutcome::Finished(_) => finished = true,
                            ChunkOutcome::Empty => {}
                        }
                    }
                }
                SseLine::Ignore => {}
            }
        }
    }

    let total_ns = start.elapsed().as_nanos() as u64;

    if !finished {
        return Err(ProbeError::StreamTruncated {
            tokens_received: tokens.len() as u32,
        });
    }

    Ok(RequestTiming::new(tokens, total_ns))
}
