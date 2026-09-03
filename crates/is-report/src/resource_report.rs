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
    /// Wall-clock UTC unix-epoch nanoseconds of the ADR-003 reference
    /// instant, captured at run start (ADR-013). Maps the relative
    /// `elapsed_ns` timeline onto absolute time, enabling the offline
    /// join with driver-side step boundaries. `None` on reports
    /// produced before ADR-013.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_instant_unix_ns: Option<u64>,
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
    /// The raw speculative-decoding timeline scraped during the sampling
    /// window, if `--metrics-endpoint` was supplied (ADR-016). This is
    /// the campaign path: a speculative run is driven by an external
    /// load generator against a server started with a speculative
    /// config, and inferscope attaches to its PID (ADR-016 D6).
    ///
    /// `None` when no endpoint was configured; empty when the engine was
    /// not speculating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_timeline: Option<is_core::SpecTimeline>,
    /// Derived per-step trajectory attribution over the sampling
    /// window, if a step file was supplied and the join was valid
    /// (ADR-013). `None` when no steps were provided, the anchor is
    /// absent, no GPU energy basis existed, or a counter regressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory: Option<crate::trajectory::TrajectoryMetrics>,
    /// Version of the serialized report schema (ADR-014 D7).
    ///
    /// Carried on both report shapes so a reader need not know which
    /// of the two it holds before it can tell whether the document
    /// predates multi-engine support. Always written by this build as
    /// [`crate::metrics::REPORT_SCHEMA_VERSION`]; `None` only on
    /// reports written before ADR-014.
    #[serde(default)]
    pub schema_version: Option<u32>,
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
            reference_instant_unix_ns: None,
            pid: 4242,
            include_descendants: false,
            sample_period_ms: 100,
            duration_secs: 30,
            resource: None,
            gpu: None,
            phase_timeline: None,
            spec_timeline: None,
            phase_energy: Some(PhaseEnergyMetrics {
                prefill_ns_delta: Some(14493),
                decode_ns_delta: Some(28432),
                prompt_tokens_delta: 196,
                generation_tokens_delta: 38,
                energy_prefill_by_time_mj: Some(33764),
                energy_decode_by_time_mj: Some(66236),
                energy_prefill_by_tokens_mj: 83761,
                energy_decode_by_tokens_mj: 16239,
                phase_energy_divergence: Some(-0.4999714),
            }),
            trajectory: None,
            schema_version: Some(crate::metrics::REPORT_SCHEMA_VERSION),
        };
        let json = render_resource_json(&report).unwrap();
        let back: ResourceReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }

    #[test]
    fn resource_report_without_phase_fields_omits_them_in_json() {
        let report = ResourceReport {
            reference_instant_unix_ns: None,
            pid: 1,
            include_descendants: false,
            sample_period_ms: 100,
            duration_secs: 1,
            resource: None,
            gpu: None,
            phase_timeline: None,
            spec_timeline: None,
            phase_energy: None,
            trajectory: None,
            schema_version: None,
        };
        let json = render_resource_json(&report).unwrap();
        assert!(!json.contains("phase_timeline"));
        assert!(!json.contains("spec_timeline"));
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
