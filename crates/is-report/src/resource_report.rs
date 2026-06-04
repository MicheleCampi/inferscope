//! Resource-only report for sample-only mode.
//!
//! Unlike [`crate::Report`], which models a full profiling run (a
//! probe with per-token timing plus optional resource/GPU samples),
//! a `ResourceReport` models a standalone resource sampling window:
//! inferscope attached to an already-running process for a fixed
//! duration and recorded its CPU/RSS and (optionally) per-device GPU
//! usage, WITHOUT generating any inference load itself.
//!
//! This is the artifact produced by `--sample-only`, intended for
//! profiling a server while an external load generator (e.g. AIPerf)
//! drives traffic. See ADR-009.
//!
//! Like [`crate::Report`], this type carries `gpu` as a plain
//! `Option<GpuMetrics>` with no feature gate: the GPU feature lives
//! in the `inferscope` binary and `is-sysmon`, not in `is-report`.
//! Callers without the feature simply pass `None`.

use serde::{Deserialize, Serialize};

use crate::metrics::{GpuMetrics, ResourceMetrics};

/// A standalone resource-sampling report (no timing, no probe).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceReport {
    /// PID that was monitored.
    pub pid: u32,
    /// Whether direct children were aggregated into the PID's metrics.
    pub include_descendants: bool,
    /// Sampling period in milliseconds.
    pub sample_period_ms: u64,
    /// Requested sampling duration in seconds.
    pub duration_secs: u64,
    /// Derived process resource metrics. `None` if no samples were taken.
    pub resource: Option<ResourceMetrics>,
    /// Derived per-device GPU metrics. `None` when GPU sampling was not
    /// requested, the feature is absent, or NVML was unavailable.
    pub gpu: Option<GpuMetrics>,
}

/// Render a `ResourceReport` as pretty JSON.
pub fn render_resource_json(report: &ResourceReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}
