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

use crate::metrics::{GpuMetrics, PhaseEnergyMetrics, ResourceMetrics};

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
    /// The raw per-phase timeline scraped from the engine's Prometheus
    /// endpoint during the sampling window, if `--metrics-endpoint` was
    /// supplied (ADR-012). `None` when no endpoint was configured or no
    /// scrape succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_timeline: Option<is_core::PhaseTimeline>,
    /// Derived per-phase energy attribution over the window, apportioning
    /// the sampled device energy across prefill and decode (ADR-012).
    /// `None` when no phase timeline was scraped, a counter regressed, no
    /// energy was sampled, or either apportionment basis had a zero delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_energy: Option<PhaseEnergyMetrics>,
}

/// Render a `ResourceReport` as pretty JSON.
pub fn render_resource_json(report: &ResourceReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_report_with_phase_energy_survives_json_round_trip() {
        let report = ResourceReport {
            pid: 4242,
            include_descendants: false,
            sample_period_ms: 100,
            duration_secs: 30,
            resource: None,
            gpu: None,
            phase_timeline: None,
            phase_energy: Some(PhaseEnergyMetrics {
                prefill_ns_delta: 14493,
                decode_ns_delta: 28432,
                prompt_tokens_delta: 196,
                generation_tokens_delta: 38,
                energy_prefill_by_time_mj: 33764,
                energy_decode_by_time_mj: 66236,
                energy_prefill_by_tokens_mj: 83761,
                energy_decode_by_tokens_mj: 16239,
                phase_energy_divergence: -0.4999714,
            }),
        };
        let json = render_resource_json(&report).unwrap();
        let back: ResourceReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }

    #[test]
    fn resource_report_without_phase_fields_omits_them_in_json() {
        let report = ResourceReport {
            pid: 1,
            include_descendants: false,
            sample_period_ms: 100,
            duration_secs: 1,
            resource: None,
            gpu: None,
            phase_timeline: None,
            phase_energy: None,
        };
        let json = render_resource_json(&report).unwrap();
        assert!(!json.contains("phase_timeline"));
        assert!(!json.contains("phase_energy"));
        // And a pre-ADR-012 JSON without the fields still deserialises.
        let legacy = serde_json::to_string(&serde_json::json!({
            "pid": 1,
            "include_descendants": false,
            "sample_period_ms": 100,
            "duration_secs": 1,
            "resource": null,
            "gpu": null
        }))
        .unwrap();
        let back: ResourceReport = serde_json::from_str(&legacy).unwrap();
        assert_eq!(back, report);
    }
}
