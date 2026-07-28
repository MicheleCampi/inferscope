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
//! - [`trajectory`]: driver-side step ingestion for agentic
//!   trajectories. See ADR-013.
//! - [`otel`] (feature `otel-export`): OpenTelemetry export of the
//!   derived metrics as OTLP/HTTP spans. See ADR-008.

pub mod derive;
pub mod error;
pub mod metrics;
pub mod render;
pub mod resource_report;
pub mod trajectory;

#[cfg(feature = "otel-export")]
pub mod otel;

pub use derive::{
    derive_efficiency, derive_gpu, derive_kvcache, derive_phase_energy, derive_resource,
    derive_timing,
};
pub use error::ReportError;
pub use metrics::{
    EfficiencyMetrics, GpuMetrics, HitRateProvenance, KvCacheMetrics, LatencyDistribution, Report,
    ResourceMetrics, TimingMetrics, REPORT_SCHEMA_VERSION,
};
pub use render::{render_json, render_text};
pub use resource_report::{render_resource_json, ResourceReport};
pub use trajectory::{
    derive_trajectory, derive_trajectory_from_timelines, parse_steps, DropReason, DroppedStep,
    StepFileError, StepKind, StepMetrics, StepRecord, TrajectoryMetrics,
};

#[cfg(feature = "otel-export")]
pub use otel::{export_to_otel, OtelExportError};
