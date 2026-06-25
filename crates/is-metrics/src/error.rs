//! Error types for the Prometheus metric source.
//!
//! The variants are organised by failure stage, the same way
//! [`is_sysmon`]'s error type is, so a caller can tell a transport
//! failure (the endpoint is unreachable, or returned a non-success
//! status) from a parse failure (the body did not contain the
//! `vllm:prefix_cache_*` series in the expected form).
//!
//! Per ADR-011 the scrape loop is best-effort: a single failed tick
//! is swallowed and the timeline continues. These errors are what a
//! single scrape *attempt* returns; the loop decides what to do with
//! them.

/// Errors that can occur while scraping a `/metrics` endpoint.
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    /// The HTTP request to the metrics endpoint failed: the host was
    /// unreachable, the connection timed out, or the transport layer
    /// errored. Carries the underlying `reqwest` diagnostic.
    #[error("metrics request failed: {source}")]
    Http {
        /// The underlying transport error.
        #[source]
        source: reqwest::Error,
    },

    /// The endpoint responded, but with a non-success HTTP status.
    /// The status code is recorded so a caller can tell a 404 (wrong
    /// path) from a 503 (engine not ready yet).
    #[error("metrics endpoint returned status {status}")]
    Status {
        /// The HTTP status code returned by the endpoint.
        status: u16,
    },

    /// The response body could not be parsed as Prometheus
    /// text-exposition content, or did not contain a series that was
    /// required. `detail` describes what was expected and missing.
    #[error("failed to parse metrics body: {detail}")]
    Parse {
        /// A human-readable description of the parse failure.
        detail: String,
    },
}
