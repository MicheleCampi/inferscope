//! Report generation for inferscope.
//!
//! `is-report` is the pure presentation layer. It takes the raw
//! signals captured by [`is_core::RequestTiming`] and
//! [`is_core::ResourceTimeline`], derives metrics from them, and
//! renders the result as either plain text for terminal viewing
//! or JSON for programmatic consumption.
//!
//! This crate does no I/O. Every operation is a pure function of
//! its inputs.
//!
//! The crate is split into:
//!
//! - [`error`]: the failure modes that can occur during derivation.
//! - [`metrics`]: the derived metric types — timing, resource, and
//!   the combined [`metrics::Report`].
//! - [`derive`]: the functions that compute metrics from the raw
//!   signals.
//! - [`render`]: text and JSON rendering of a report.
//! - [`otel`] (feature `otel-export`): OpenTelemetry export of the
//!   derived metrics as OTLP/HTTP spans. See ADR-008.

pub mod derive;
pub mod error;
pub mod metrics;
pub mod render;
pub mod resource_report;

#[cfg(feature = "otel-export")]
pub mod otel;

pub use derive::{derive_gpu, derive_resource, derive_timing};
pub use error::ReportError;
pub use metrics::{GpuMetrics, LatencyDistribution, Report, ResourceMetrics, TimingMetrics};
pub use render::{render_json, render_text};
pub use resource_report::{render_resource_json, ResourceReport};

#[cfg(feature = "otel-export")]
pub use otel::{export_to_otel, OtelExportError};
