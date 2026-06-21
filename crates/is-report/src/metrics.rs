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
}

#[cfg(test)]
mod tests {
    use super::*;
    use is_core::{ResourceSample, ResourceTimeline, TokenArrival};

    fn sample_report() -> Report {
        Report {
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
        };

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: Report = serde_json::from_str(&json).expect("deserialize");

        assert!(restored.resource_timeline.is_none());
        assert!(restored.resource.is_none());
        assert_eq!(restored.timing.token_count, 0);
    }
}
