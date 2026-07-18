//! Derived metric types for the report layer.
//!
//! These types describe the *computed* view of a probe run: the
//! same data as the raw signals, but interpreted into the
//! quantities a reader actually wants to see.
//!
//! Per ADR-004 the derived metrics live side by side with the raw
//! signals in the JSON output, so a consumer can either read the
//! derived numbers or recompute them differently from the raw
//! data.

use is_core::{RequestTiming, ResourceTimeline};
use serde::{Deserialize, Serialize};

use is_core::EnergySource;

/// Distribution summary for a set of inter-token latency deltas.
///
/// All values are in nanoseconds. Percentiles are computed with
/// nearest-rank rounding up; with small `count` the high
/// percentiles approach `max` and should be read accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyDistribution {
    /// Number of inter-token intervals in the distribution.
    pub count: u32,
    /// Arithmetic mean of the intervals, in nanoseconds.
    pub mean_ns: u64,
    /// 50th percentile (median), in nanoseconds.
    pub p50_ns: u64,
    /// 95th percentile, in nanoseconds.
    pub p95_ns: u64,
    /// 99th percentile, in nanoseconds.
    pub p99_ns: u64,
    /// Maximum, in nanoseconds.
    pub max_ns: u64,
}

/// Timing metrics derived from a [`RequestTiming`].
///
/// `tokens_per_second` is `f64` so this type cannot derive `Eq`;
/// `PartialEq` is enough for tests that need it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimingMetrics {
    /// Number of tokens recorded.
    pub token_count: u32,
    /// Time-to-first-token. `None` if the request produced no tokens.
    pub ttft_ns: Option<u64>,
    /// Total wall-clock duration of the request.
    pub total_latency_ns: u64,
    /// Generation rate during streaming (excludes TTFT, per ADR-004).
    /// `None` when fewer than two tokens were produced.
    pub tokens_per_second: Option<f64>,
    /// Distribution of inter-token latencies. `None` when fewer
    /// than two tokens were produced.
    pub inter_token_latency: Option<LatencyDistribution>,
}

/// Resource metrics derived from a [`ResourceTimeline`].
///
/// `cpu_mean_percent` is `f64` so this type cannot derive `Eq`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ResourceMetrics {
    /// Number of samples in the timeline.
    pub sample_count: u32,
    /// Minimum resident set size observed, in bytes.
    pub rss_min_bytes: u64,
    /// Maximum resident set size observed, in bytes.
    pub rss_max_bytes: u64,
    /// Arithmetic mean of resident set size, in bytes.
    pub rss_mean_bytes: u64,
    /// Resident set size at the last sample, in bytes.
    pub rss_final_bytes: u64,
    /// Mean CPU utilisation as a percentage. May exceed 100 on
    /// multi-threaded processes. `None` if fewer than two samples
    /// were taken or wall time spanned is zero.
    pub cpu_mean_percent: Option<f64>,
    /// Minimum thread count observed.
    pub thread_min: u32,
    /// Maximum thread count observed.
    pub thread_max: u32,
}

/// Per-device GPU metrics derived from a [`GpuTimeline`].
///
/// Each entry summarises the samples for one `device_index`.
/// Introduced in v0.3.0 per [ADR-007](../../docs/adr/007-per-device-gpu-metrics.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDeviceMetrics {
    /// Index of the GPU device as reported by NVML.
    pub device_index: u32,
    /// Number of samples for this device in the timeline.
    pub sample_count: u32,
    /// Minimum VRAM in use across this device's samples, in bytes.
    pub memory_used_min_bytes: u64,
    /// Maximum VRAM in use across this device's samples, in bytes.
    pub memory_used_max_bytes: u64,
    /// Arithmetic mean VRAM in use across this device's samples, in bytes.
    pub memory_used_mean_bytes: u64,
    /// Total VRAM capacity for this specific device.
    pub memory_total_bytes: u64,
    /// Minimum SM utilisation observed for this device, in percent.
    pub utilization_min_percent: u8,
    /// Maximum SM utilisation observed for this device, in percent.
    pub utilization_max_percent: u8,
    /// Mean SM utilisation for this device across its samples, in percent.
    pub utilization_mean_percent: u8,
    /// Maximum chip temperature observed for this device, in degrees Celsius.
    pub temperature_max_celsius: u32,
    /// Maximum power draw observed for this device, in milliwatts.
    pub power_max_milliwatts: u32,
    /// Mean power draw for this device across its samples, in milliwatts.
    pub power_mean_milliwatts: u32,
    /// Energy consumed by this device over the window, in millijoules
    /// (ADR-010). `None` when neither the NVML counter nor a power
    /// integral could be computed (e.g. a single sample). The unit is
    /// integer millijoules to keep this type `Eq`; joule conversion
    /// happens at the efficiency/render layer.
    pub energy_millijoules: Option<u64>,
    /// How `energy_millijoules` was obtained: the NVML counter, or the
    /// trapezoidal power integral fallback. `None` iff energy is `None`.
    pub energy_source: Option<EnergySource>,
}

/// GPU metrics derived from a [`GpuTimeline`].
///
/// Top-level fields are cluster-wide aggregates across all devices
/// and all samples; the `per_device` field (introduced in v0.3.0)
/// breaks these out by `device_index`. See
/// [ADR-007](../../docs/adr/007-per-device-gpu-metrics.md) for the
/// rationale.
///
/// `*_mean_percent` and `*_mean_milliwatts` fields at the top level
/// are computed across all samples without weighting; on a multi-GPU
/// run the mean reflects total observation time per device equally.
/// For per-device means without cross-device mixing, read `per_device`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuMetrics {
    /// Number of samples in the timeline.
    pub sample_count: u32,
    /// Number of distinct GPU devices that appear in the timeline.
    pub device_count: u32,
    /// Minimum VRAM in use across all samples, in bytes.
    pub memory_used_min_bytes: u64,
    /// Maximum VRAM in use across all samples, in bytes.
    pub memory_used_max_bytes: u64,
    /// Arithmetic mean VRAM in use across all samples, in bytes.
    pub memory_used_mean_bytes: u64,
    /// Total VRAM capacity reported for the first device.
    /// On a multi-GPU host with mixed cards this only reflects one;
    /// consumers needing per-device totals inspect the raw samples.
    pub memory_total_bytes: u64,
    /// Minimum SM utilisation observed, in percent.
    pub utilization_min_percent: u8,
    /// Maximum SM utilisation observed, in percent.
    pub utilization_max_percent: u8,
    /// Mean SM utilisation across all samples, in percent.
    pub utilization_mean_percent: u8,
    /// Maximum chip temperature observed, in degrees Celsius.
    pub temperature_max_celsius: u32,
    /// Maximum power draw observed, in milliwatts.
    pub power_max_milliwatts: u32,
    /// Mean power draw across all samples, in milliwatts.
    pub power_mean_milliwatts: u32,
    /// Total energy over the window across all devices, in millijoules
    /// (ADR-010): the sum of the per-device energies. `None` when no
    /// device produced an energy figure.
    pub energy_millijoules: Option<u64>,
    /// The energy source for the aggregate. `Counter` if every
    /// contributing device used the counter; `IntegratedFallback` if
    /// any device fell back to the integral (the weakest link governs,
    /// so a consumer never over-trusts a mixed aggregate). `None` iff
    /// energy is `None`.
    pub energy_source: Option<EnergySource>,
    /// Per-device breakdown of the aggregates above, one entry per
    /// `device_index`. Empty for single-device runs is impossible —
    /// the timeline always has at least one device, but consumers
    /// should treat absence as equivalent to a single-element vector.
    pub per_device: Vec<GpuDeviceMetrics>,
}

/// Energy-efficiency metrics derived from GPU energy and token count
/// (ADR-010).
///
/// All fields are `f64`, so this type is `PartialEq` but not `Eq`.
///
/// `tokens_per_watt` and `tokens_per_joule` are the *same physical
/// quantity* expressed two ways: tokens/(W*s) = tokens/J, so with time
/// in seconds the two are numerically identical. inferscope exposes both
/// because "tokens per watt" is the market's term while "tokens per
/// joule" makes the energy basis explicit, but they are not independent
/// signals -- both come from one calculation (token_count / energy).
///
/// `energy_source` is carried through from the GPU metrics so a consumer
/// knows whether this efficiency rests on the NVML counter or on the
/// integrated-power fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EfficiencyMetrics {
    /// Total GPU energy over the window, in joules.
    pub energy_joules: f64,
    /// Energy cost per output token, in millijoules.
    pub energy_per_token_mj: f64,
    /// Output tokens produced per joule of GPU energy.
    pub tokens_per_joule: f64,
    /// Output tokens per watt. Numerically equal to `tokens_per_joule`.
    pub tokens_per_watt: f64,
    /// Whether the underlying energy came from the NVML counter or the
    /// integrated-power fallback.
    pub energy_source: EnergySource,
}

/// Derived KV-cache metrics for one probe run (ADR-011).
///
/// `vllm:prefix_cache_hits` and `vllm:prefix_cache_queries` are
/// monotonic counters, so the meaningful figure is the delta across the
/// probe window, not an absolute. `hits_delta` and `queries_delta` are
/// the differences between the last and first scrape; `hit_rate` is
/// `hits_delta / queries_delta`.
///
/// This is `None` on the report when the window is invalid: the counter
/// regressed (the engine reset mid-run, so a delta would be meaningless)
/// or no queries occurred in the window (`queries_delta == 0`, no rate to
/// form). See [`crate::derive::derive_kvcache`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KvCacheMetrics {
    /// Cached token-blocks served over the window: the rise in
    /// `vllm:prefix_cache_hits` from the first scrape to the last.
    pub hits_delta: u64,
    /// Token-blocks queried over the window: the rise in
    /// `vllm:prefix_cache_queries`. The denominator of the rate.
    pub queries_delta: u64,
    /// Fraction of queried token-blocks served from cache over the
    /// window: `hits_delta / queries_delta`, in `0.0..=1.0`.
    pub hit_rate: f64,
}

/// Per-phase energy attribution over the scrape window (ADR-012).
///
/// This is an **apportionment of device-level NVML energy, not a
/// measurement of phase energy** — and that caveat holds for *both*
/// apportionments equally. Under interleaved execution (continuous
/// batching, chunked prefill) no temporal cut isolates a phase on the
/// device counter; `prefill + decode` is moreover only a fraction of
/// inference time, itself a fraction of wall-clock. Both figures are
/// projections of the same total energy onto two different bases.
///
/// The total energy apportioned is the aggregate figure from
/// [`GpuMetrics`] (counter-preferred, trapezoidal fallback). Each basis
/// splits it conservatively: the prefill side is rounded from the share,
/// the decode side is the remainder, so `prefill + decode` equals the
/// total exactly for each basis.
///
/// Withheld (`None`) when the window has fewer than two samples, any
/// cumulative counter regresses (engine reset), no energy figure exists
/// to apportion, or either basis has a zero delta (a zero phase-time
/// delta makes the time-share undefined; a zero token delta makes the
/// token-share undefined). Because the divergence needs both shares, a
/// missing apportionment withholds the whole struct rather than emitting
/// half of it — the same all-or-nothing discipline as [`KvCacheMetrics`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhaseEnergyMetrics {
    /// Window delta of cumulative prefill time, nanoseconds.
    pub prefill_ns_delta: u64,
    /// Window delta of cumulative decode time, nanoseconds.
    pub decode_ns_delta: u64,
    /// Window delta of cumulative prompt (prefill) tokens.
    pub prompt_tokens_delta: u64,
    /// Window delta of cumulative generation (decode) tokens.
    pub generation_tokens_delta: u64,
    /// Energy apportioned to prefill by time-share
    /// `prefill_ns / (prefill_ns + decode_ns)`, millijoules.
    pub energy_prefill_by_time_mj: u64,
    /// Energy apportioned to decode by time-share, the remainder of the
    /// total after the prefill time-share, millijoules.
    pub energy_decode_by_time_mj: u64,
    /// Energy apportioned to prefill by token-share
    /// `prompt_tok / (prompt_tok + gen_tok)`, millijoules.
    pub energy_prefill_by_tokens_mj: u64,
    /// Energy apportioned to decode by token-share, the remainder of the
    /// total after the prefill token-share, millijoules.
    pub energy_decode_by_tokens_mj: u64,
    /// The first-class signal: prefill time-share minus prefill
    /// token-share. Quantifies the energy-per-token asymmetry between
    /// compute-bound prefill and memory-bound decode. Negative when
    /// decode spreads the energy budget over proportionally more tokens
    /// than its time-share would suggest.
    pub phase_energy_divergence: f64,
}

/// The full report for one probe run: raw signals plus derived
/// metrics, packaged together so the JSON output is a single
/// self-contained document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// The raw token timing as captured by the probe.
    pub request_timing: RequestTiming,
    /// The raw resource timeline as captured by the sysmon, if any.
    pub resource_timeline: Option<ResourceTimeline>,
    /// The raw GPU timeline as captured by the GPU sampler, if any.
    pub gpu_timeline: Option<is_core::GpuTimeline>,
    /// Derived timing metrics.
    pub timing: TimingMetrics,
    /// Derived resource metrics, if a resource timeline was available.
    pub resource: Option<ResourceMetrics>,
    /// Derived GPU metrics, if a GPU timeline was available.
    pub gpu: Option<GpuMetrics>,
    /// Derived energy-efficiency metrics, if both an energy figure
    /// and a positive token count were available (ADR-010). `None`
    /// when energy could not be measured or no tokens were produced.
    pub efficiency: Option<EfficiencyMetrics>,
    /// The raw KV-cache timeline as scraped from the engine's
    /// Prometheus endpoint, if any (ADR-011). `None` when no metrics
    /// endpoint was configured or no scrape succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kvcache_timeline: Option<is_core::KvCacheTimeline>,
    /// Derived KV-cache metrics, if a valid window was scraped
    /// (ADR-011). `None` when no timeline was available or the window
    /// was invalid (counter regression, or zero queries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kvcache: Option<KvCacheMetrics>,
    /// The raw per-phase timeline as scraped from the engine's
    /// Prometheus endpoint, if any (ADR-012). `None` when no metrics
    /// endpoint was configured or no scrape succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_timeline: Option<is_core::PhaseTimeline>,
    /// Derived per-phase energy attribution, if a valid window was
    /// scraped and an energy figure existed to apportion (ADR-012).
    /// `None` when no timeline was available, a counter regressed, no
    /// energy was measured, or either apportionment basis had a zero
    /// delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_energy: Option<PhaseEnergyMetrics>,
    /// Wall-clock UTC unix-epoch nanoseconds of the ADR-003 reference
    /// instant, captured at run start (ADR-013). Maps the relative
    /// `elapsed_ns` timeline onto absolute time, enabling the offline
    /// join with driver-side step boundaries. `None` on reports
    /// produced before ADR-013.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_instant_unix_ns: Option<u64>,
    /// Derived per-step trajectory attribution, if a step file was
    /// supplied and the join was valid (ADR-013). `None` when no
    /// steps were provided, the report lacks the wall-clock anchor,
    /// no GPU energy basis existed, or a counter regressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory: Option<crate::trajectory::TrajectoryMetrics>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use is_core::{ResourceSample, ResourceTimeline, TokenArrival};

    #[test]
    fn anchor_absent_is_skipped_and_defaults_to_none() {
        let report = sample_report();
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("reference_instant_unix_ns"));
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reference_instant_unix_ns, None);
    }

    #[test]
    fn anchor_roundtrips_when_present() {
        let mut report = sample_report();
        report.reference_instant_unix_ns = Some(1_752_000_000_000_000_000);
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.reference_instant_unix_ns,
            Some(1_752_000_000_000_000_000)
        );
    }

    fn sample_report() -> Report {
        Report {
            reference_instant_unix_ns: None,
            request_timing: RequestTiming::new(
                vec![
                    TokenArrival::new(0, 412_000_000),
                    TokenArrival::new(1, 458_000_000),
                ],
                470_000_000,
            ),
            resource_timeline: Some(ResourceTimeline {
                samples: vec![ResourceSample {
                    elapsed_ns: 50_000_000,
                    rss_bytes: 612 * 1024 * 1024,
                    cpu_user_jiffies: 100,
                    cpu_system_jiffies: 5,
                    thread_count: 8,
                }],
                sample_period_ns: 50_000_000,
            }),
            gpu_timeline: None,
            timing: TimingMetrics {
                token_count: 2,
                ttft_ns: Some(412_000_000),
                total_latency_ns: 470_000_000,
                tokens_per_second: Some(21.74),
                inter_token_latency: Some(LatencyDistribution {
                    count: 1,
                    mean_ns: 46_000_000,
                    p50_ns: 46_000_000,
                    p95_ns: 46_000_000,
                    p99_ns: 46_000_000,
                    max_ns: 46_000_000,
                }),
            },
            resource: Some(ResourceMetrics {
                sample_count: 1,
                rss_min_bytes: 612 * 1024 * 1024,
                rss_max_bytes: 612 * 1024 * 1024,
                rss_mean_bytes: 612 * 1024 * 1024,
                rss_final_bytes: 612 * 1024 * 1024,
                cpu_mean_percent: None,
                thread_min: 8,
                thread_max: 8,
            }),
            gpu: None,
            efficiency: None,
            kvcache_timeline: None,
            kvcache: None,
            phase_timeline: None,
            phase_energy: None,
            trajectory: None,
        }
    }

    #[test]
    fn report_survives_json_round_trip() {
        let original = sample_report();
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: Report = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.timing.token_count, 2);
        assert_eq!(restored.timing.ttft_ns, Some(412_000_000));
        assert_eq!(restored.timing.total_latency_ns, 470_000_000);
        assert_eq!(
            restored.resource_timeline.as_ref().unwrap().samples.len(),
            1
        );
        let dist = restored.timing.inter_token_latency.unwrap();
        assert_eq!(dist.count, 1);
        assert_eq!(dist.mean_ns, 46_000_000);
    }

    #[test]
    fn report_with_no_resource_timeline_round_trips() {
        let original = Report {
            reference_instant_unix_ns: None,
            request_timing: RequestTiming::new(vec![], 0),
            resource_timeline: None,
            gpu_timeline: None,
            timing: TimingMetrics {
                token_count: 0,
                ttft_ns: None,
                total_latency_ns: 0,
                tokens_per_second: None,
                inter_token_latency: None,
            },
            resource: None,
            gpu: None,
            efficiency: None,
            kvcache_timeline: None,
            kvcache: None,
            phase_timeline: None,
            phase_energy: None,
            trajectory: None,
        };

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: Report = serde_json::from_str(&json).expect("deserialize");

        assert!(restored.resource_timeline.is_none());
        assert!(restored.resource.is_none());
        assert_eq!(restored.timing.token_count, 0);
    }

    #[test]
    fn report_with_efficiency_round_trips() {
        let mut original = sample_report();
        original.efficiency = Some(EfficiencyMetrics {
            energy_joules: 51.5,
            energy_per_token_mj: 17_166.7,
            tokens_per_joule: 0.058,
            tokens_per_watt: 0.058,
            energy_source: EnergySource::Counter,
        });
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: Report = serde_json::from_str(&json).expect("deserialize");
        let eff = restored.efficiency.expect("efficiency survives round-trip");
        assert_eq!(eff.energy_joules, 51.5);
        assert_eq!(eff.tokens_per_joule, eff.tokens_per_watt);
        assert_eq!(eff.energy_source, EnergySource::Counter);
    }
}
