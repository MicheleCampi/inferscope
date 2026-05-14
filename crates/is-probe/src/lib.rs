//! Probe client for inferscope.
//!
//! `is-probe` drives an inference engine through its OpenAI-compatible
//! HTTP API and captures per-token timing. It sends a streamed chat
//! completion request, records when each token arrives, and produces a
//! [`is_core::RequestTiming`] holding the raw signal.
//!
//! The crate is split into:
//!
//! - [`config`]: what to ask the engine to do — endpoint, model,
//!   prompt, request parameters.
//! - [`error`]: the failure modes of probing — transport failure,
//!   engine-side HTTP error, malformed or truncated stream.
//! - the streaming probe itself, which ties a [`config::ProbeConfig`]
//!   to a measured [`is_core::RequestTiming`]. (Lands later in W2.)

pub mod config;
pub mod error;

pub use config::ProbeConfig;
pub use error::ProbeError;
