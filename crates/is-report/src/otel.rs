//! OpenTelemetry export of inferscope reports.
//!
//! This module is gated behind the `otel-export` Cargo feature. When
//! enabled, [`export_to_otel`] takes a derived [`crate::Report`] and
//! emits it as a single OTLP/HTTP trace: one root span representing
//! the inference run, with the per-token arrivals attached as span
//! events and the derived aggregates attached as span attributes.
//!
//! Design choices recorded in `docs/adr/008-opentelemetry-export.md`:
//!
//! - **One root span per run.** The run is the unit of work; events
//!   on the root span carry per-token timing.
//! - **Token arrivals as events, not child spans.** A token arrival
//!   is a timestamp, not a sub-operation with duration. Span events
//!   are the OTel-semantic-correct shape.
//! - **GPU and resource aggregates as attributes.** Lossless summary
//!   without overwhelming the trace with timeline samples.
//! - **OTLP over HTTP/protobuf, not gRPC.** Smaller dependency
//!   footprint, traverses corporate firewalls without special config.
//! - **Export failure does not fail the run.** Observability is
//!   secondary to the profiling result. Errors surface via
//!   `Result<(), OtelExportError>` for the caller to log.

use thiserror::Error;

use crate::Report;

/// Errors that can occur while exporting a report to OpenTelemetry.
///
/// Construction is split by failure phase so a caller can distinguish
/// configuration errors (likely user-recoverable) from transport errors
/// (likely transient or out of the caller's control).
#[derive(Debug, Error)]
pub enum OtelExportError {
    /// Setting up the tracer provider failed. Typically a malformed
    /// endpoint URL or a missing required environment variable.
    #[error("failed to initialise OTLP exporter: {0}")]
    SetupFailed(String),

    /// The exporter could not flush the span to the configured
    /// endpoint. Common causes: collector unreachable, network
    /// timeout, collector rejected the payload.
    #[error("failed to export span to {endpoint}: {message}")]
    ExportFailed { endpoint: String, message: String },

    /// Not-yet-implemented. Returned by the commit-1 skeleton; the
    /// real implementation lands in commit 2 of this series.
    #[error("OpenTelemetry export not yet implemented (commit-1 skeleton)")]
    NotImplemented,
}

/// Exports a derived [`Report`] as an OpenTelemetry trace to the
/// configured OTLP/HTTP endpoint.
///
/// `endpoint` is expected to be the **base URL** of the OTLP receiver,
/// not the full traces path. For a local OpenTelemetry Collector with
/// default settings this is `http://localhost:4318`. The function
/// appends `/v1/traces` internally.
///
/// On success returns `Ok(())`. On failure returns one of the
/// variants of [`OtelExportError`]. The caller is expected to log the
/// error and continue; export failure should never fail the calling
/// run.
///
/// This is the **commit-1 skeleton** — it validates the API surface
/// and dependency wiring without yet emitting any trace. The real
/// implementation lands in commit 2.
pub fn export_to_otel(_report: &Report, _endpoint: &str) -> Result<(), OtelExportError> {
    Err(OtelExportError::NotImplemented)
}
