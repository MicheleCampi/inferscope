//! Compute derived metrics from raw timing and resource signals.
//!
//! The functions in this module are pure: they take borrowed raw
//! data and return derived metric values, with no I/O, no logging,
//! no state. Per ADR-004 the computations live here rather than at
//! collection time so that the raw signal remains the source of
//! truth.

use is_core::{KvCacheTimeline, RequestTiming, ResourceTimeline};

use crate::error::ReportError;
use crate::metrics::{KvCacheMetrics, LatencyDistribution, ResourceMetrics, TimingMetrics};

/// Computes derived timing metrics from a [`RequestTiming`].
///
/// Returns a fully populated [`TimingMetrics`] whose
/// `Option`-typed fields are `None` exactly when the underlying
/// signal does not support them — `ttft_ns` is `None` for an
/// empty token list, and `tokens_per_second` and
/// `inter_token_latency` are `None` when fewer than two tokens
/// were generated.
pub fn derive_timing(timing: &RequestTiming) -> TimingMetrics {
    let tokens = &timing.tokens;
    let token_count = tokens.len() as u32;

    let ttft_ns = tokens.first().map(|t| t.elapsed_ns);

    let (tokens_per_second, inter_token_latency) = if tokens.len() < 2 {
        (None, None)
    } else {
        let first = tokens.first().unwrap().elapsed_ns;
        let last = tokens.last().unwrap().elapsed_ns;
        let span_ns = last - first;
        // (N - 1) intervals between N tokens, divided by the wall
        // time spanned from first to last. ADR-004 records the
        // rationale for excluding TTFT here.
        let tps = if span_ns > 0 {
            let intervals = (tokens.len() - 1) as f64;
            let seconds = span_ns as f64 / 1_000_000_000.0;
            Some(intervals / seconds)
        } else {
            None
        };

        let mut deltas: Vec<u64> = tokens
            .windows(2)
            .map(|pair| pair[1].elapsed_ns - pair[0].elapsed_ns)
            .collect();
        deltas.sort_unstable();

        let count = deltas.len() as u32;
        let sum: u64 = deltas.iter().sum();
        let mean_ns = sum / deltas.len() as u64;

        let dist = LatencyDistribution {
            count,
            mean_ns,
            p50_ns: percentile_nearest_rank(&deltas, 50),
            p95_ns: percentile_nearest_rank(&deltas, 95),
            p99_ns: percentile_nearest_rank(&deltas, 99),
            max_ns: *deltas.last().unwrap(),
        };

        (tps, Some(dist))
    };

    TimingMetrics {
        token_count,
        ttft_ns,
        total_latency_ns: timing.total_ns,
        tokens_per_second,
        inter_token_latency,
    }
}

/// Computes derived resource metrics from a [`ResourceTimeline`].
///
/// Returns `Ok(None)` if the timeline is empty (no samples to
/// aggregate). Returns `Err(ReportError::UnknownClockTick)` if
/// `_SC_CLK_TCK` cannot be queried.
pub fn derive_resource(
    timeline: &ResourceTimeline,
) -> Result<Option<ResourceMetrics>, ReportError> {
    if timeline.samples.is_empty() {
        return Ok(None);
    }

    let samples = &timeline.samples;
    let sample_count = samples.len() as u32;

    let rss_min_bytes = samples.iter().map(|s| s.rss_bytes).min().unwrap();
    let rss_max_bytes = samples.iter().map(|s| s.rss_bytes).max().unwrap();
    let rss_sum: u128 = samples.iter().map(|s| s.rss_bytes as u128).sum();
    let rss_mean_bytes = (rss_sum / samples.len() as u128) as u64;
    let rss_final_bytes = samples.last().unwrap().rss_bytes;

    let thread_min = samples.iter().map(|s| s.thread_count).min().unwrap();
    let thread_max = samples.iter().map(|s| s.thread_count).max().unwrap();

    let cpu_mean_percent = if samples.len() < 2 {
        None
    } else {
        let clk_tck = clock_tick_hz()?;
        let first = samples.first().unwrap();
        let last = samples.last().unwrap();

        let jiffy_delta = (last.cpu_user_jiffies + last.cpu_system_jiffies)
            .saturating_sub(first.cpu_user_jiffies + first.cpu_system_jiffies);
        let wall_ns = last.elapsed_ns.saturating_sub(first.elapsed_ns);

        if wall_ns == 0 {
            None
        } else {
            let cpu_seconds = jiffy_delta as f64 / clk_tck as f64;
            let wall_seconds = wall_ns as f64 / 1_000_000_000.0;
            Some((cpu_seconds / wall_seconds) * 100.0)
        }
    };

    Ok(Some(ResourceMetrics {
        sample_count,
        rss_min_bytes,
        rss_max_bytes,
        rss_mean_bytes,
        rss_final_bytes,
        cpu_mean_percent,
        thread_min,
        thread_max,
    }))
}

/// Reads `_SC_CLK_TCK` via `libc::sysconf`.
///
/// On all platforms inferscope targets the value is positive and
/// typically 100. A non-positive return indicates the platform
/// could not answer, and we surface that as `UnknownClockTick`
/// rather than fabricating a default.
fn clock_tick_hz() -> Result<u64, ReportError> {
    // SAFETY: sysconf is a thread-safe POSIX query. _SC_CLK_TCK is
    // a standard constant. The function reads no caller-provided
    // memory.
    let raw = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if raw <= 0 {
        Err(ReportError::UnknownClockTick)
    } else {
        Ok(raw as u64)
    }
}

/// Returns the value at the given percentile (1..=100) of a sorted
/// slice, using the nearest-rank method with rounding up.
fn percentile_nearest_rank(sorted: &[u64], percentile: u32) -> u64 {
    debug_assert!(!sorted.is_empty(), "percentile of empty slice");
    debug_assert!(
        (1..=100).contains(&percentile),
        "percentile must be in 1..=100"
    );
    let n = sorted.len();
    // nearest-rank index, 1-based, rounding up
    let rank = ((percentile as usize * n) + 99) / 100;
    // Convert to 0-based and clamp to last index just in case.
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

/// Derives energy-efficiency metrics from aggregate GPU energy and the
/// output token count (ADR-010).
///
/// Returns `None` when there is nothing to divide: no energy was
/// measured, the energy is zero, or no tokens were produced. A single
/// founding calculation drives the family — tokens per joule — and
/// `tokens_per_watt` is returned as the same value, since with time in
/// seconds tokens/(W*s) = tokens/J. Exposing both as one number rather
/// than two independent computes keeps them coherent by construction.
pub fn derive_efficiency(
    energy_millijoules: Option<u64>,
    energy_source: Option<is_core::EnergySource>,
    token_count: u32,
) -> Option<crate::metrics::EfficiencyMetrics> {
    let mj = energy_millijoules?;
    let source = energy_source?;
    if mj == 0 || token_count == 0 {
        return None;
    }
    let tokens = token_count as f64;
    let energy_joules = mj as f64 / 1000.0;
    let tokens_per_joule = tokens / energy_joules;
    Some(crate::metrics::EfficiencyMetrics {
        energy_joules,
        energy_per_token_mj: mj as f64 / tokens,
        tokens_per_joule,
        // Identity: tokens/(W*s) = tokens/J (time in seconds).
        tokens_per_watt: tokens_per_joule,
        energy_source: source,
    })
}

/// Derives KV-cache metrics from a [`KvCacheTimeline`] (ADR-011).
///
/// The window figure is a delta across monotonic counters: the rise in
/// hits and queries from the first scrape to the last. Returns `None`
/// when no valid rate can be formed:
///
/// - the timeline has fewer than two samples (no window to difference);
/// - the counter regressed — a later reading is below an earlier one,
///   meaning the engine reset mid-run, so a delta would be meaningless
///   (the same guard `compute_energy_delta` applies to the energy
///   counter in ADR-010);
/// - no queries occurred over the window (`queries_delta == 0`), leaving
///   nothing to divide by.
///
/// `hit_rate` is `hits_delta / queries_delta`. A well-formed window has
/// `hits_delta <= queries_delta` (you cannot hit more blocks than you
/// queried), so the rate lands in `0.0..=1.0`; the function does not
/// clamp, leaving any upstream anomaly visible rather than masked.
pub fn derive_kvcache(timeline: &KvCacheTimeline) -> Option<KvCacheMetrics> {
    let first = timeline.samples.first()?;
    let last = timeline.samples.last()?;

    // Fewer than two distinct samples: no window to difference. (first
    // and last being the same element yields zero deltas, caught below
    // by the queries_delta == 0 guard, but an explicit length check
    // makes the intent clear.)
    if timeline.samples.len() < 2 {
        return None;
    }

    // Counter regression guard: a monotonic counter that went backwards
    // means the engine reset within the window. No trustworthy delta.
    if last.hits < first.hits || last.queries < first.queries {
        return None;
    }

    let hits_delta = last.hits - first.hits;
    let queries_delta = last.queries - first.queries;

    if queries_delta == 0 {
        return None;
    }

    Some(KvCacheMetrics {
        hits_delta,
        queries_delta,
        hit_rate: hits_delta as f64 / queries_delta as f64,
    })
}

/// Trapezoidal integral of a device's power samples over time,
/// returning energy in millijoules.
///
/// This is the ADR-010 fallback used when the NVML energy counter is
/// unavailable for a device. The samples must be for a single device
/// and ordered by `elapsed_ns` (the GPU sampler emits them that way).
///
/// Method: energy = sum over adjacent sample pairs of the trapezoid
/// area 0.5 * (P_i + P_{i+1}) * dt. Power is in milliwatts and time in
/// nanoseconds, so each term is mW * ns = 1e-12 J = 1e-9 mJ; the running
/// sum is divided by 1e9 to land in millijoules. Fewer than two samples
/// gives no interval to integrate and returns 0.
fn integrate_power_trapezoidal(samples: &[&is_core::GpuSample]) -> u64 {
    if samples.len() < 2 {
        return 0;
    }
    let mut area_mw_ns: f64 = 0.0;
    for pair in samples.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let dt_ns = b.elapsed_ns.saturating_sub(a.elapsed_ns) as f64;
        let mean_power_mw = (a.power_draw_milliwatts as f64 + b.power_draw_milliwatts as f64) / 2.0;
        area_mw_ns += mean_power_mw * dt_ns;
    }
    // mW * ns -> mJ: divide by 1e9.
    (area_mw_ns / 1.0e9) as u64
}

/// Computes derived GPU metrics from a [`GpuTimeline`].
///
/// Returns `Ok(None)` if the timeline is empty. Aggregations span
/// every sample regardless of `device_index`; a multi-GPU run
/// produces one set of summary statistics. Consumers needing
/// per-device breakdowns inspect the raw `gpu_timeline.samples`
/// directly (the timeline keeps device_index on every sample).
///
/// Mean values are arithmetic means over all samples without
/// weighting. For utilisation in particular, this means a
/// quiet GPU and a busy one in the same run contribute equally
/// to the mean; consumers who care about per-device patterns
/// look at the raw data.
pub fn derive_gpu(timeline: &is_core::GpuTimeline) -> Option<crate::metrics::GpuMetrics> {
    if timeline.samples.is_empty() {
        return None;
    }

    let samples = &timeline.samples;
    let sample_count = samples.len() as u32;
    let device_count = timeline.device_indices().len() as u32;

    // VRAM aggregations.
    let memory_used_min_bytes = samples.iter().map(|s| s.memory_used_bytes).min().unwrap();
    let memory_used_max_bytes = samples.iter().map(|s| s.memory_used_bytes).max().unwrap();
    let memory_sum: u128 = samples.iter().map(|s| s.memory_used_bytes as u128).sum();
    let memory_used_mean_bytes = (memory_sum / samples.len() as u128) as u64;

    // VRAM total from the first sample. ADR-005 note: on a
    // multi-GPU host with cards of different total VRAM this is
    // only the first device's total; the field documents that
    // limitation.
    let memory_total_bytes = samples.first().unwrap().memory_total_bytes;

    // Utilisation aggregations (u8 in 0..=100 by ADR-005 contract).
    let utilization_min_percent = samples.iter().map(|s| s.utilization_percent).min().unwrap();
    let utilization_max_percent = samples.iter().map(|s| s.utilization_percent).max().unwrap();
    let util_sum: u32 = samples.iter().map(|s| s.utilization_percent as u32).sum();
    let utilization_mean_percent = (util_sum / samples.len() as u32) as u8;

    // Temperature: only max is informative — we are looking for
    // thermal events. Min and mean would be dominated by long
    // idle periods at session boundaries.
    let temperature_max_celsius = samples.iter().map(|s| s.temperature_celsius).max().unwrap();

    // Power: peak and mean.
    let power_max_milliwatts = samples
        .iter()
        .map(|s| s.power_draw_milliwatts)
        .max()
        .unwrap();
    let power_sum: u64 = samples.iter().map(|s| s.power_draw_milliwatts as u64).sum();
    let power_mean_milliwatts = (power_sum / samples.len() as u64) as u32;

    // Per-device aggregates (ADR-007). One entry per distinct
    // device_index in the timeline, ordered ascending.
    let mut per_device: Vec<crate::metrics::GpuDeviceMetrics> = timeline
        .device_indices()
        .iter()
        .map(|&d| {
            let dev_samples: Vec<&_> = samples.iter().filter(|s| s.device_index == d).collect();
            let dev_sample_count = dev_samples.len() as u32;

            let dev_mem_min = dev_samples
                .iter()
                .map(|s| s.memory_used_bytes)
                .min()
                .unwrap();
            let dev_mem_max = dev_samples
                .iter()
                .map(|s| s.memory_used_bytes)
                .max()
                .unwrap();
            let dev_mem_sum: u128 = dev_samples
                .iter()
                .map(|s| s.memory_used_bytes as u128)
                .sum();
            let dev_mem_mean = (dev_mem_sum / dev_samples.len() as u128) as u64;
            let dev_mem_total = dev_samples.first().unwrap().memory_total_bytes;

            let dev_util_min = dev_samples
                .iter()
                .map(|s| s.utilization_percent)
                .min()
                .unwrap();
            let dev_util_max = dev_samples
                .iter()
                .map(|s| s.utilization_percent)
                .max()
                .unwrap();
            let dev_util_sum: u32 = dev_samples
                .iter()
                .map(|s| s.utilization_percent as u32)
                .sum();
            let dev_util_mean = (dev_util_sum / dev_samples.len() as u32) as u8;

            let dev_temp_max = dev_samples
                .iter()
                .map(|s| s.temperature_celsius)
                .max()
                .unwrap();
            let dev_power_max = dev_samples
                .iter()
                .map(|s| s.power_draw_milliwatts)
                .max()
                .unwrap();
            let dev_power_sum: u64 = dev_samples
                .iter()
                .map(|s| s.power_draw_milliwatts as u64)
                .sum();
            let dev_power_mean = (dev_power_sum / dev_samples.len() as u64) as u32;

            // ADR-010 energy: prefer the NVML counter for this device;
            // fall back to the trapezoidal power integral otherwise.
            let counter_mj = timeline.energy.as_ref().and_then(|es| {
                es.iter()
                    .find(|e| e.device_index == d && e.source == is_core::EnergySource::Counter)
                    .map(|e| e.energy_millijoules)
            });
            let (dev_energy_mj, dev_energy_source) = match counter_mj {
                Some(mj) => (Some(mj), Some(is_core::EnergySource::Counter)),
                None => {
                    let integ = integrate_power_trapezoidal(&dev_samples);
                    if integ > 0 {
                        (Some(integ), Some(is_core::EnergySource::IntegratedFallback))
                    } else {
                        (None, None)
                    }
                }
            };

            crate::metrics::GpuDeviceMetrics {
                device_index: d,
                sample_count: dev_sample_count,
                memory_used_min_bytes: dev_mem_min,
                memory_used_max_bytes: dev_mem_max,
                memory_used_mean_bytes: dev_mem_mean,
                memory_total_bytes: dev_mem_total,
                utilization_min_percent: dev_util_min,
                utilization_max_percent: dev_util_max,
                utilization_mean_percent: dev_util_mean,
                temperature_max_celsius: dev_temp_max,
                power_max_milliwatts: dev_power_max,
                power_mean_milliwatts: dev_power_mean,
                energy_millijoules: dev_energy_mj,
                energy_source: dev_energy_source,
            }
        })
        .collect();
    per_device.sort_by_key(|d| d.device_index);

    // ADR-010 aggregate energy: sum of per-device energies. The source
    // is Counter only if every contributing device used the counter;
    // if any device fell back to the integral, the aggregate is marked
    // IntegratedFallback (the weakest link governs, so the aggregate is
    // never presented as counter-grade when it is partly estimated).
    let contributing: Vec<&crate::metrics::GpuDeviceMetrics> = per_device
        .iter()
        .filter(|d| d.energy_millijoules.is_some())
        .collect();
    let (energy_millijoules, energy_source) = if contributing.is_empty() {
        (None, None)
    } else {
        let total: u64 = contributing
            .iter()
            .map(|d| d.energy_millijoules.unwrap())
            .sum();
        let all_counter = contributing
            .iter()
            .all(|d| d.energy_source == Some(is_core::EnergySource::Counter));
        let source = if all_counter {
            is_core::EnergySource::Counter
        } else {
            is_core::EnergySource::IntegratedFallback
        };
        (Some(total), Some(source))
    };

    Some(crate::metrics::GpuMetrics {
        sample_count,
        device_count,
        memory_used_min_bytes,
        memory_used_max_bytes,
        memory_used_mean_bytes,
        memory_total_bytes,
        utilization_min_percent,
        utilization_max_percent,
        utilization_mean_percent,
        temperature_max_celsius,
        power_max_milliwatts,
        power_mean_milliwatts,
        energy_millijoules,
        energy_source,
        per_device,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use is_core::{ResourceSample, ResourceTimeline, TokenArrival};

    // ----- derive_timing -----

    #[test]
    fn derive_timing_for_empty_request() {
        let timing = RequestTiming::new(vec![], 0);
        let m = derive_timing(&timing);
        assert_eq!(m.token_count, 0);
        assert_eq!(m.ttft_ns, None);
        assert_eq!(m.total_latency_ns, 0);
        assert_eq!(m.tokens_per_second, None);
        assert!(m.inter_token_latency.is_none());
    }

    #[test]
    fn derive_timing_for_single_token() {
        let timing = RequestTiming::new(vec![TokenArrival::new(0, 412_000_000)], 412_500_000);
        let m = derive_timing(&timing);
        assert_eq!(m.token_count, 1);
        assert_eq!(m.ttft_ns, Some(412_000_000));
        assert_eq!(m.total_latency_ns, 412_500_000);
        // Fewer than two tokens: rate and distribution are undefined.
        assert_eq!(m.tokens_per_second, None);
        assert!(m.inter_token_latency.is_none());
    }

    #[test]
    fn derive_timing_excludes_ttft_from_tokens_per_second() {
        // 5 tokens, TTFT 1s, then 4 more tokens spaced 100 ms apart
        // (so 4 intervals over 400 ms). Generation rate is
        // 4 intervals / 0.4 s = 10 tokens/s. If we mistakenly
        // included TTFT, the rate would be 4 / 1.4 = ~2.86 tokens/s.
        let tokens = vec![
            TokenArrival::new(0, 1_000_000_000),
            TokenArrival::new(1, 1_100_000_000),
            TokenArrival::new(2, 1_200_000_000),
            TokenArrival::new(3, 1_300_000_000),
            TokenArrival::new(4, 1_400_000_000),
        ];
        let timing = RequestTiming::new(tokens, 1_500_000_000);
        let m = derive_timing(&timing);
        let tps = m.tokens_per_second.expect("rate should be defined");
        assert!(
            (tps - 10.0).abs() < 0.001,
            "expected ~10 tokens/s, got {tps}"
        );
    }

    #[test]
    fn derive_timing_inter_token_distribution_with_uniform_spacing() {
        // 4 tokens spaced 50 ms apart -> 3 intervals each 50 ms.
        let tokens = vec![
            TokenArrival::new(0, 100_000_000),
            TokenArrival::new(1, 150_000_000),
            TokenArrival::new(2, 200_000_000),
            TokenArrival::new(3, 250_000_000),
        ];
        let timing = RequestTiming::new(tokens, 260_000_000);
        let m = derive_timing(&timing);
        let dist = m.inter_token_latency.unwrap();
        assert_eq!(dist.count, 3);
        assert_eq!(dist.mean_ns, 50_000_000);
        assert_eq!(dist.p50_ns, 50_000_000);
        assert_eq!(dist.max_ns, 50_000_000);
    }

    #[test]
    fn derive_timing_inter_token_distribution_with_spike() {
        // 5 intervals: 10 10 10 10 100 ms. Sorted: 10 10 10 10 100.
        // p50 = index ceil(2.5)=3 -> sorted[2] = 10 ms.
        // p95 = index ceil(4.75)=5 -> sorted[4] = 100 ms.
        // max = 100 ms. mean = 28 ms.
        let mut tokens = vec![TokenArrival::new(0, 100_000_000)];
        let mut t = 100_000_000u64;
        for (i, delta) in [10, 10, 10, 10, 100].iter().enumerate() {
            t += delta * 1_000_000;
            tokens.push(TokenArrival::new((i + 1) as u32, t));
        }
        let timing = RequestTiming::new(tokens, t + 1_000_000);
        let m = derive_timing(&timing);
        let dist = m.inter_token_latency.unwrap();
        assert_eq!(dist.count, 5);
        assert_eq!(dist.mean_ns, 28_000_000);
        assert_eq!(dist.p50_ns, 10_000_000);
        assert_eq!(dist.p95_ns, 100_000_000);
        assert_eq!(dist.max_ns, 100_000_000);
    }

    // ----- derive_resource -----

    fn sample(elapsed_ns: u64, rss: u64, ujif: u64, sjif: u64, threads: u32) -> ResourceSample {
        ResourceSample {
            elapsed_ns,
            rss_bytes: rss,
            cpu_user_jiffies: ujif,
            cpu_system_jiffies: sjif,
            thread_count: threads,
        }
    }

    #[test]
    fn derive_resource_for_empty_timeline_returns_none() {
        let tl = ResourceTimeline::new(50_000_000);
        let m = derive_resource(&tl).expect("derive should not error");
        assert!(m.is_none());
    }

    #[test]
    fn derive_resource_with_single_sample_omits_cpu() {
        let mut tl = ResourceTimeline::new(50_000_000);
        tl.push(sample(50_000_000, 1024, 100, 5, 4));
        let m = derive_resource(&tl)
            .expect("derive should not error")
            .unwrap();
        assert_eq!(m.sample_count, 1);
        assert_eq!(m.rss_min_bytes, 1024);
        assert_eq!(m.rss_max_bytes, 1024);
        assert_eq!(m.rss_mean_bytes, 1024);
        assert_eq!(m.rss_final_bytes, 1024);
        assert_eq!(m.thread_min, 4);
        assert_eq!(m.thread_max, 4);
        // CPU is undefined with a single sample.
        assert_eq!(m.cpu_mean_percent, None);
    }

    #[test]
    fn derive_resource_rss_aggregations() {
        let mut tl = ResourceTimeline::new(50_000_000);
        tl.push(sample(50_000_000, 100, 0, 0, 1));
        tl.push(sample(100_000_000, 300, 0, 0, 1));
        tl.push(sample(150_000_000, 200, 0, 0, 1));
        let m = derive_resource(&tl)
            .expect("derive should not error")
            .unwrap();
        assert_eq!(m.sample_count, 3);
        assert_eq!(m.rss_min_bytes, 100);
        assert_eq!(m.rss_max_bytes, 300);
        assert_eq!(m.rss_mean_bytes, 200);
        // final is the last in time order, regardless of magnitude.
        assert_eq!(m.rss_final_bytes, 200);
    }

    #[test]
    fn derive_resource_cpu_utilisation_within_plausible_range() {
        // Two samples 1 second apart with 50 jiffies of CPU work
        // between them. On a 100 Hz system that is 0.5 CPU-seconds
        // over 1 wall-second = 50%. Verifying the exact value
        // would tie the test to the host's CLK_TCK; we instead
        // assert plausibility.
        let mut tl = ResourceTimeline::new(50_000_000);
        tl.push(sample(0, 100, 100, 0, 1));
        tl.push(sample(1_000_000_000, 100, 150, 0, 1));
        let m = derive_resource(&tl)
            .expect("derive should not error")
            .unwrap();
        let cpu = m.cpu_mean_percent.expect("two samples -> defined");
        assert!(
            (0.0..=10_000.0).contains(&cpu),
            "CPU utilisation out of plausible range: {cpu}"
        );
    }

    // ----- percentile -----

    #[test]
    fn percentile_basics() {
        let sorted = [10u64, 20, 30, 40, 50];
        assert_eq!(percentile_nearest_rank(&sorted, 50), 30);
        assert_eq!(percentile_nearest_rank(&sorted, 95), 50);
        assert_eq!(percentile_nearest_rank(&sorted, 99), 50);
        assert_eq!(percentile_nearest_rank(&sorted, 100), 50);
    }

    // ----- derive_gpu -----

    fn gpu_sample(
        elapsed_ns: u64,
        device_index: u32,
        mem_used: u64,
        util: u8,
        temp: u32,
        power_mw: u32,
    ) -> is_core::GpuSample {
        is_core::GpuSample {
            elapsed_ns,
            device_index,
            memory_used_bytes: mem_used,
            memory_total_bytes: 80 * 1024 * 1024 * 1024,
            utilization_percent: util,
            temperature_celsius: temp,
            power_draw_milliwatts: power_mw,
        }
    }

    #[test]
    fn derive_gpu_returns_none_on_empty_timeline() {
        let tl = is_core::GpuTimeline::new(50_000_000);
        assert!(derive_gpu(&tl).is_none());
    }

    #[test]
    fn derive_gpu_single_sample_collapses_to_that_sample() {
        let mut tl = is_core::GpuTimeline::new(50_000_000);
        tl.push(gpu_sample(0, 0, 1_000_000_000, 50, 45, 200_000));
        let m = derive_gpu(&tl).expect("non-empty");
        assert_eq!(m.sample_count, 1);
        assert_eq!(m.device_count, 1);
        assert_eq!(m.memory_used_min_bytes, 1_000_000_000);
        assert_eq!(m.memory_used_max_bytes, 1_000_000_000);
        assert_eq!(m.memory_used_mean_bytes, 1_000_000_000);
        assert_eq!(m.utilization_min_percent, 50);
        assert_eq!(m.utilization_max_percent, 50);
        assert_eq!(m.utilization_mean_percent, 50);
        assert_eq!(m.temperature_max_celsius, 45);
        assert_eq!(m.power_max_milliwatts, 200_000);
        assert_eq!(m.power_mean_milliwatts, 200_000);
    }

    #[test]
    fn derive_gpu_multiple_samples_aggregates_min_max_mean() {
        let mut tl = is_core::GpuTimeline::new(50_000_000);
        // Three samples on device 0 with rising VRAM and utilisation.
        tl.push(gpu_sample(0, 0, 1_000_000_000, 10, 40, 100_000));
        tl.push(gpu_sample(50_000_000, 0, 2_000_000_000, 50, 50, 200_000));
        tl.push(gpu_sample(100_000_000, 0, 3_000_000_000, 90, 60, 300_000));
        let m = derive_gpu(&tl).expect("non-empty");
        assert_eq!(m.sample_count, 3);
        assert_eq!(m.device_count, 1);
        // VRAM: min=1G, max=3G, mean=2G.
        assert_eq!(m.memory_used_min_bytes, 1_000_000_000);
        assert_eq!(m.memory_used_max_bytes, 3_000_000_000);
        assert_eq!(m.memory_used_mean_bytes, 2_000_000_000);
        // Util: min=10, max=90, mean=50.
        assert_eq!(m.utilization_min_percent, 10);
        assert_eq!(m.utilization_max_percent, 90);
        assert_eq!(m.utilization_mean_percent, 50);
        // Temperature peak.
        assert_eq!(m.temperature_max_celsius, 60);
        // Power: max=300mW, mean=200mW.
        assert_eq!(m.power_max_milliwatts, 300_000);
        assert_eq!(m.power_mean_milliwatts, 200_000);
    }

    #[test]
    fn derive_gpu_multi_gpu_counts_devices_correctly() {
        let mut tl = is_core::GpuTimeline::new(50_000_000);
        // One tick of a 4-GPU machine.
        tl.push(gpu_sample(0, 0, 1_000_000_000, 50, 45, 200_000));
        tl.push(gpu_sample(0, 1, 1_000_000_000, 50, 45, 200_000));
        tl.push(gpu_sample(0, 2, 1_000_000_000, 50, 45, 200_000));
        tl.push(gpu_sample(0, 3, 1_000_000_000, 50, 45, 200_000));
        let m = derive_gpu(&tl).expect("non-empty");
        assert_eq!(m.sample_count, 4);
        assert_eq!(m.device_count, 4);
    }

    #[test]
    fn derive_gpu_populates_per_device_for_multi_gpu_run() {
        let mut tl = is_core::GpuTimeline::new(50_000_000);
        // Simulate a TP=2 run on a 4-GPU host: GPU 0 and 1 carry work,
        // GPU 2 and 3 sit at idle floor. Two sample ticks per device
        // (t=0 and t=50ms), eight samples total.
        for elapsed in [0u64, 50_000_000] {
            tl.push(gpu_sample(elapsed, 0, 2_000_000_000, 30, 50, 150_000));
            tl.push(gpu_sample(elapsed, 1, 3_000_000_000, 40, 55, 160_000));
            tl.push(gpu_sample(elapsed, 2, 0, 0, 33, 34_000));
            tl.push(gpu_sample(elapsed, 3, 0, 0, 34, 32_000));
        }
        let m = derive_gpu(&tl).expect("non-empty");
        assert_eq!(m.sample_count, 8);
        assert_eq!(m.device_count, 4);
        // per_device must have exactly one entry per distinct device,
        // sorted ascending by device_index.
        assert_eq!(m.per_device.len(), 4);
        assert_eq!(m.per_device[0].device_index, 0);
        assert_eq!(m.per_device[1].device_index, 1);
        assert_eq!(m.per_device[2].device_index, 2);
        assert_eq!(m.per_device[3].device_index, 3);
        // Each device contributed two samples.
        for d in &m.per_device {
            assert_eq!(d.sample_count, 2);
        }
        // Busy device (GPU 1) shows the populated values, not zero.
        assert_eq!(m.per_device[1].memory_used_max_bytes, 3_000_000_000);
        assert_eq!(m.per_device[1].utilization_mean_percent, 40);
        assert_eq!(m.per_device[1].power_mean_milliwatts, 160_000);
        assert_eq!(m.per_device[1].temperature_max_celsius, 55);
        // Idle device (GPU 3) shows the idle floor — the whole point
        // of per-device: this asymmetry is visible, not averaged away.
        assert_eq!(m.per_device[3].memory_used_max_bytes, 0);
        assert_eq!(m.per_device[3].utilization_mean_percent, 0);
        assert_eq!(m.per_device[3].power_mean_milliwatts, 32_000);
    }

    // ----- integrate_power_trapezoidal -----
    #[test]
    fn integral_constant_power_equals_power_times_time() {
        // 100 W (=100_000 mW) held for 2 s (=2e9 ns) -> 200 J = 200_000 mJ.
        // Trapezoidal rule is exact for a constant function regardless
        // of how many intermediate samples we place.
        let owned = [
            gpu_sample(0, 0, 0, 0, 0, 100_000),
            gpu_sample(500_000_000, 0, 0, 0, 0, 100_000),
            gpu_sample(1_000_000_000, 0, 0, 0, 0, 100_000),
            gpu_sample(2_000_000_000, 0, 0, 0, 0, 100_000),
        ];
        let refs: Vec<&is_core::GpuSample> = owned.iter().collect();
        assert_eq!(integrate_power_trapezoidal(&refs), 200_000);
    }

    #[test]
    fn integral_linear_ramp_equals_mean_power_times_time() {
        // Linear ramp 0 -> 200_000 mW over 2 s. Mean power 100_000 mW,
        // so energy = 100 W * 2 s = 200_000 mJ. Trapezoid is exact on
        // a linear function.
        let owned = [
            gpu_sample(0, 0, 0, 0, 0, 0),
            gpu_sample(1_000_000_000, 0, 0, 0, 0, 100_000),
            gpu_sample(2_000_000_000, 0, 0, 0, 0, 200_000),
        ];
        let refs: Vec<&is_core::GpuSample> = owned.iter().collect();
        assert_eq!(integrate_power_trapezoidal(&refs), 200_000);
    }

    #[test]
    fn integral_fewer_than_two_samples_is_zero() {
        let owned = [gpu_sample(0, 0, 0, 0, 0, 100_000)];
        let refs: Vec<&is_core::GpuSample> = owned.iter().collect();
        assert_eq!(integrate_power_trapezoidal(&refs), 0);
        assert_eq!(integrate_power_trapezoidal(&[]), 0);
    }

    // ----- derive_gpu energy (ADR-010) -----
    #[test]
    fn derive_gpu_prefers_counter_energy_when_present() {
        // Device 0 has a counter reading; it must win over the integral.
        let mut tl = is_core::GpuTimeline::new(1_000_000_000);
        tl.push(gpu_sample(0, 0, 0, 0, 0, 100_000));
        tl.push(gpu_sample(2_000_000_000, 0, 0, 0, 0, 100_000));
        // Integral over these would be 200_000 mJ; counter says 51_500.
        tl.energy = Some(vec![is_core::DeviceEnergy {
            device_index: 0,
            energy_millijoules: 51_500,
            source: is_core::EnergySource::Counter,
        }]);
        let m = derive_gpu(&tl).expect("non-empty");
        assert_eq!(m.per_device[0].energy_millijoules, Some(51_500));
        assert_eq!(
            m.per_device[0].energy_source,
            Some(is_core::EnergySource::Counter)
        );
        // Aggregate of a single counter device is counter-grade.
        assert_eq!(m.energy_millijoules, Some(51_500));
        assert_eq!(m.energy_source, Some(is_core::EnergySource::Counter));
    }

    #[test]
    fn derive_gpu_falls_back_to_integral_without_counter() {
        // No timeline.energy -> integrate. Constant 100 W for 2 s.
        let mut tl = is_core::GpuTimeline::new(1_000_000_000);
        tl.push(gpu_sample(0, 0, 0, 0, 0, 100_000));
        tl.push(gpu_sample(2_000_000_000, 0, 0, 0, 0, 100_000));
        let m = derive_gpu(&tl).expect("non-empty");
        assert_eq!(m.per_device[0].energy_millijoules, Some(200_000));
        assert_eq!(
            m.per_device[0].energy_source,
            Some(is_core::EnergySource::IntegratedFallback)
        );
    }

    #[test]
    fn derive_gpu_aggregate_is_fallback_grade_if_any_device_estimated() {
        // Device 0: counter. Device 1: no counter -> integral.
        let mut tl = is_core::GpuTimeline::new(1_000_000_000);
        tl.push(gpu_sample(0, 0, 0, 0, 0, 100_000));
        tl.push(gpu_sample(2_000_000_000, 0, 0, 0, 0, 100_000));
        tl.push(gpu_sample(0, 1, 0, 0, 0, 50_000));
        tl.push(gpu_sample(2_000_000_000, 1, 0, 0, 0, 50_000));
        tl.energy = Some(vec![is_core::DeviceEnergy {
            device_index: 0,
            energy_millijoules: 60_000,
            source: is_core::EnergySource::Counter,
        }]);
        let m = derive_gpu(&tl).expect("non-empty");
        // Device 1 integral: 50 W * 2 s = 100_000 mJ.
        assert_eq!(m.per_device[1].energy_millijoules, Some(100_000));
        // Aggregate = 60_000 + 100_000, marked fallback (weakest link).
        assert_eq!(m.energy_millijoules, Some(160_000));
        assert_eq!(
            m.energy_source,
            Some(is_core::EnergySource::IntegratedFallback)
        );
    }

    #[test]
    fn derive_gpu_aggregate_is_counter_grade_when_all_counter() {
        let mut tl = is_core::GpuTimeline::new(1_000_000_000);
        tl.push(gpu_sample(0, 0, 0, 0, 0, 100_000));
        tl.push(gpu_sample(2_000_000_000, 0, 0, 0, 0, 100_000));
        tl.push(gpu_sample(0, 1, 0, 0, 0, 100_000));
        tl.push(gpu_sample(2_000_000_000, 1, 0, 0, 0, 100_000));
        tl.energy = Some(vec![
            is_core::DeviceEnergy {
                device_index: 0,
                energy_millijoules: 40_000,
                source: is_core::EnergySource::Counter,
            },
            is_core::DeviceEnergy {
                device_index: 1,
                energy_millijoules: 45_000,
                source: is_core::EnergySource::Counter,
            },
        ]);
        let m = derive_gpu(&tl).expect("non-empty");
        assert_eq!(m.energy_millijoules, Some(85_000));
        assert_eq!(m.energy_source, Some(is_core::EnergySource::Counter));
    }

    // ----- derive_efficiency (ADR-010) -----
    #[test]
    fn efficiency_known_values() {
        // 200_000 mJ = 200 J, 1000 tokens.
        let e = derive_efficiency(Some(200_000), Some(is_core::EnergySource::Counter), 1000)
            .expect("some efficiency");
        assert_eq!(e.energy_joules, 200.0);
        assert_eq!(e.energy_per_token_mj, 200.0); // 200_000 / 1000
        assert_eq!(e.tokens_per_joule, 5.0); // 1000 / 200
        assert_eq!(e.tokens_per_watt, 5.0); // identity
        assert_eq!(e.energy_source, is_core::EnergySource::Counter);
    }

    #[test]
    fn efficiency_tokens_per_watt_equals_tokens_per_joule() {
        // Non-round values: the identity must hold exactly, because the
        // two come from one computation, not two.
        let e = derive_efficiency(
            Some(73_456),
            Some(is_core::EnergySource::IntegratedFallback),
            337,
        )
        .expect("some efficiency");
        assert_eq!(e.tokens_per_watt, e.tokens_per_joule);
        assert_eq!(e.energy_source, is_core::EnergySource::IntegratedFallback);
    }

    #[test]
    fn efficiency_none_without_energy() {
        assert!(derive_efficiency(None, None, 1000).is_none());
    }

    #[test]
    fn efficiency_none_on_zero_energy_or_zero_tokens() {
        assert!(derive_efficiency(Some(0), Some(is_core::EnergySource::Counter), 1000).is_none());
        assert!(
            derive_efficiency(Some(200_000), Some(is_core::EnergySource::Counter), 0).is_none()
        );
    }

    // ----- derive_kvcache (ADR-011) -----

    fn kv_sample(elapsed_ns: u64, hits: u64, queries: u64) -> is_core::KvCacheSample {
        is_core::KvCacheSample {
            elapsed_ns,
            hits,
            queries,
        }
    }

    #[test]
    fn derive_kvcache_window_delta_and_rate() {
        // Cache warms over the window: first scrape 10/40, last 96/196.
        // hits_delta = 86, queries_delta = 156, rate = 86/156 = 0.5513.
        let mut tl = is_core::KvCacheTimeline::new(1_000_000_000);
        tl.push(kv_sample(0, 10, 40));
        tl.push(kv_sample(1_000_000_000, 48, 120));
        tl.push(kv_sample(2_000_000_000, 96, 196));
        let m = derive_kvcache(&tl).expect("valid window");
        assert_eq!(m.hits_delta, 86);
        assert_eq!(m.queries_delta, 156);
        assert!((m.hit_rate - (86.0 / 156.0)).abs() < 1e-12);
    }

    #[test]
    fn derive_kvcache_two_samples_minimum() {
        // Exactly two samples is a valid window.
        let mut tl = is_core::KvCacheTimeline::new(1_000_000_000);
        tl.push(kv_sample(0, 0, 0));
        tl.push(kv_sample(1_000_000_000, 96, 196));
        let m = derive_kvcache(&tl).expect("valid window");
        assert_eq!(m.hits_delta, 96);
        assert_eq!(m.queries_delta, 196);
        assert!((m.hit_rate - (96.0 / 196.0)).abs() < 1e-12);
    }

    #[test]
    fn derive_kvcache_empty_timeline_is_none() {
        let tl = is_core::KvCacheTimeline::new(1_000_000_000);
        assert!(derive_kvcache(&tl).is_none());
    }

    #[test]
    fn derive_kvcache_single_sample_is_none() {
        // One scrape: no window to difference.
        let mut tl = is_core::KvCacheTimeline::new(1_000_000_000);
        tl.push(kv_sample(0, 96, 196));
        assert!(derive_kvcache(&tl).is_none());
    }

    #[test]
    fn derive_kvcache_counter_regression_is_none() {
        // The engine reset mid-window: last reading below first.
        // A delta would be meaningless, so no metric.
        let mut tl = is_core::KvCacheTimeline::new(1_000_000_000);
        tl.push(kv_sample(0, 96, 196));
        tl.push(kv_sample(1_000_000_000, 5, 12));
        assert!(derive_kvcache(&tl).is_none());
    }

    #[test]
    fn derive_kvcache_zero_queries_delta_is_none() {
        // No queries occurred over the window: nothing to divide by.
        let mut tl = is_core::KvCacheTimeline::new(1_000_000_000);
        tl.push(kv_sample(0, 50, 100));
        tl.push(kv_sample(1_000_000_000, 50, 100));
        assert!(derive_kvcache(&tl).is_none());
    }
}
