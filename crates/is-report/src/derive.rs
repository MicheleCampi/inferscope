//! Compute derived metrics from raw timing and resource signals.
//!
//! The functions in this module are pure: they take borrowed raw
//! data and return derived metric values, with no I/O, no logging,
//! no state. Per ADR-004 the computations live here rather than at
//! collection time so that the raw signal remains the source of
//! truth.

use is_core::{RequestTiming, ResourceTimeline};

use crate::error::ReportError;
use crate::metrics::{LatencyDistribution, ResourceMetrics, TimingMetrics};

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
}
