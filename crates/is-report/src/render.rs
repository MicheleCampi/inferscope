//! Render a [`Report`] as plain ASCII text or as JSON.
//!
//! These functions are pure: they consume a borrowed report and
//! return a string. They do no I/O and no logging. Per ADR-004 the
//! text output is plain ASCII (no colour, no Unicode block
//! characters) so the report copies cleanly into issue trackers,
//! pull requests, and chat threads.

use std::fmt::Write;

use crate::metrics::{LatencyDistribution, Report, ResourceMetrics, TimingMetrics};

/// Renders the report as plain ASCII text targeted at terminal
/// reading and copy-pasting into prose contexts.
///
/// The output is organised in up to three sections:
///
/// - probe summary — token count, TTFT, generation rate, total
///   latency
/// - inter-token latency distribution — when at least two tokens
///   were produced
/// - resource usage — when a non-empty resource timeline was
///   captured
///
/// Sections that have no data (no tokens; no second token; no
/// resource timeline) are omitted rather than filled with
/// placeholder values.
pub fn render_text(report: &Report) -> String {
    let mut out = String::new();

    render_probe_summary(&mut out, &report.timing);

    if let Some(dist) = &report.timing.inter_token_latency {
        out.push('\n');
        render_inter_token_latency(&mut out, dist);
    }

    if let Some(res) = &report.resource {
        out.push('\n');
        render_resource_usage(&mut out, res);
    }
    if let Some(gpu) = &report.gpu {
        out.push('\n');
        render_gpu_usage(&mut out, gpu);
    }

    out
}

/// Renders the report as a pretty-printed JSON document.
///
/// The JSON carries both raw signals (request_timing,
/// resource_timeline) and derived metrics (timing, resource)
/// per ADR-004 — a consumer can either read the computed values
/// or recompute them from the raw data.
pub fn render_json(report: &Report) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

fn render_probe_summary(out: &mut String, timing: &TimingMetrics) {
    let _ = writeln!(out, "Probe summary");
    let _ = writeln!(out, "  Tokens generated      {}", timing.token_count);

    match timing.ttft_ns {
        Some(ns) => {
            let _ = writeln!(out, "  Time to first token   {}", format_duration_ns(ns));
        }
        None => {
            let _ = writeln!(out, "  Time to first token   (no tokens produced)");
        }
    }

    match timing.tokens_per_second {
        Some(rate) => {
            let _ = writeln!(out, "  Generation rate       {:.1} tokens/s", rate);
        }
        None => {
            let _ = writeln!(out, "  Generation rate       (not enough tokens)");
        }
    }

    let _ = writeln!(
        out,
        "  Total latency         {}",
        format_duration_ns(timing.total_latency_ns)
    );
}

fn render_inter_token_latency(out: &mut String, dist: &LatencyDistribution) {
    let _ = writeln!(out, "Inter-token latency (from {} intervals)", dist.count);
    let _ = writeln!(
        out,
        "  mean   {:>8}      max    {:>8}",
        format_duration_ns(dist.mean_ns),
        format_duration_ns(dist.max_ns),
    );
    let _ = writeln!(
        out,
        "  p50    {:>8}      p95    {:>8}",
        format_duration_ns(dist.p50_ns),
        format_duration_ns(dist.p95_ns),
    );
    let _ = writeln!(out, "  p99    {:>8}", format_duration_ns(dist.p99_ns));
}

fn render_resource_usage(out: &mut String, res: &ResourceMetrics) {
    let _ = writeln!(out, "Process resource usage ({} samples)", res.sample_count);
    let _ = writeln!(
        out,
        "  RSS                peak {}  mean {}",
        format_bytes(res.rss_max_bytes),
        format_bytes(res.rss_mean_bytes),
    );
    let _ = writeln!(
        out,
        "                     min  {}  final {}",
        format_bytes(res.rss_min_bytes),
        format_bytes(res.rss_final_bytes),
    );

    match res.cpu_mean_percent {
        Some(pct) => {
            let _ = writeln!(out, "  CPU utilization    mean {:.0}%", pct);
        }
        None => {
            let _ = writeln!(out, "  CPU utilization    (not enough samples)");
        }
    }

    if res.thread_min == res.thread_max {
        let _ = writeln!(out, "  Threads            {} throughout", res.thread_min);
    } else {
        let _ = writeln!(
            out,
            "  Threads            {} .. {}",
            res.thread_min, res.thread_max
        );
    }
}
fn render_gpu_usage(out: &mut String, gpu: &crate::metrics::GpuMetrics) {
    let device_suffix = if gpu.device_count == 1 {
        String::new()
    } else {
        format!(", {} devices", gpu.device_count)
    };
    let _ = writeln!(
        out,
        "GPU resource usage ({} samples{})",
        gpu.sample_count, device_suffix
    );
    let _ = writeln!(
        out,
        "  VRAM               peak {}  mean {}",
        format_bytes(gpu.memory_used_max_bytes),
        format_bytes(gpu.memory_used_mean_bytes),
    );
    let _ = writeln!(
        out,
        "                     min  {}  total {}",
        format_bytes(gpu.memory_used_min_bytes),
        format_bytes(gpu.memory_total_bytes),
    );
    let _ = writeln!(
        out,
        "  SM utilization     peak {}%  mean {}%  min {}%",
        gpu.utilization_max_percent, gpu.utilization_mean_percent, gpu.utilization_min_percent,
    );
    let _ = writeln!(
        out,
        "  Temperature        peak {} C",
        gpu.temperature_max_celsius
    );
    let _ = writeln!(
        out,
        "  Power draw         peak {:.1} W  mean {:.1} W",
        gpu.power_max_milliwatts as f64 / 1000.0,
        gpu.power_mean_milliwatts as f64 / 1000.0,
    );
}

/// Formats a nanosecond duration as a short human-readable string.
///
/// Switches between us/ms/s based on magnitude. Values below 1 us
/// are reported as the literal "<1 us" rather than confusing the
/// reader with fractional microseconds.
fn format_duration_ns(ns: u64) -> String {
    if ns < 1_000 {
        return "<1 us".to_string();
    }
    if ns < 1_000_000 {
        return format!("{} us", ns / 1_000);
    }
    if ns < 1_000_000_000 {
        return format!("{} ms", ns / 1_000_000);
    }
    let seconds = ns as f64 / 1_000_000_000.0;
    format!("{:.3} s", seconds)
}

/// Formats a byte count using binary prefixes (KiB, MiB, GiB).
fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    if bytes < KIB {
        format!("{} B", bytes)
    } else if bytes < MIB {
        format!("{} KiB", bytes / KIB)
    } else if bytes < GIB {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use is_core::{RequestTiming, ResourceTimeline, TokenArrival};

    fn sample_report() -> Report {
        Report {
            request_timing: RequestTiming::new(
                vec![
                    TokenArrival::new(0, 412_000_000),
                    TokenArrival::new(1, 458_000_000),
                    TokenArrival::new(2, 504_000_000),
                ],
                550_000_000,
            ),
            resource_timeline: Some(ResourceTimeline {
                samples: vec![],
                sample_period_ns: 50_000_000,
            }),
            gpu_timeline: None,
            timing: TimingMetrics {
                token_count: 3,
                ttft_ns: Some(412_000_000),
                total_latency_ns: 550_000_000,
                tokens_per_second: Some(21.74),
                inter_token_latency: Some(LatencyDistribution {
                    count: 2,
                    mean_ns: 46_000_000,
                    p50_ns: 46_000_000,
                    p95_ns: 46_000_000,
                    p99_ns: 46_000_000,
                    max_ns: 46_000_000,
                }),
            },
            resource: Some(ResourceMetrics {
                sample_count: 5,
                rss_min_bytes: 600 * 1024 * 1024,
                rss_max_bytes: 612 * 1024 * 1024,
                rss_mean_bytes: 605 * 1024 * 1024,
                rss_final_bytes: 612 * 1024 * 1024,
                cpu_mean_percent: Some(245.7),
                thread_min: 8,
                thread_max: 8,
            }),
            gpu: None,
        }
    }

    // ----- render_text -----

    #[test]
    fn text_render_includes_all_three_sections_when_data_present() {
        let r = sample_report();
        let text = render_text(&r);
        assert!(text.contains("Probe summary"));
        assert!(text.contains("Inter-token latency"));
        assert!(text.contains("Process resource usage"));
    }

    #[test]
    fn text_render_omits_inter_token_when_distribution_missing() {
        let mut r = sample_report();
        r.timing.inter_token_latency = None;
        let text = render_text(&r);
        assert!(text.contains("Probe summary"));
        assert!(!text.contains("Inter-token latency"));
    }

    #[test]
    fn text_render_omits_resource_when_missing() {
        let mut r = sample_report();
        r.resource = None;
        let text = render_text(&r);
        assert!(!text.contains("Process resource usage"));
    }

    #[test]
    fn text_render_handles_an_empty_request() {
        let r = Report {
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
        let text = render_text(&r);
        assert!(text.contains("Tokens generated      0"));
        assert!(text.contains("no tokens produced"));
        assert!(text.contains("not enough tokens"));
    }

    #[test]
    fn text_render_reports_constant_threads_compactly() {
        let r = sample_report();
        let text = render_text(&r);
        assert!(text.contains("8 throughout"));
    }

    #[test]
    fn text_render_reports_varying_threads_as_range() {
        let mut r = sample_report();
        if let Some(res) = r.resource.as_mut() {
            res.thread_min = 4;
            res.thread_max = 8;
        }
        let text = render_text(&r);
        assert!(text.contains("4 .. 8"));
    }

    // ----- render_json -----

    #[test]
    fn json_render_is_valid_json() {
        let r = sample_report();
        let json = render_json(&r).expect("json render should succeed");
        let _value: serde_json::Value =
            serde_json::from_str(&json).expect("output must be valid json");
    }

    #[test]
    fn json_render_includes_raw_signals_and_derived_metrics() {
        let r = sample_report();
        let json = render_json(&r).expect("json render should succeed");
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Per ADR-004 both layers must be present.
        assert!(value.get("request_timing").is_some());
        assert!(value.get("timing").is_some());
        assert!(value.get("resource").is_some());
    }

    // ----- formatters -----

    #[test]
    fn format_duration_sub_microsecond() {
        assert_eq!(format_duration_ns(500), "<1 us");
    }

    #[test]
    fn format_duration_microseconds() {
        assert_eq!(format_duration_ns(45_000), "45 us");
    }

    #[test]
    fn format_duration_milliseconds() {
        assert_eq!(format_duration_ns(46_000_000), "46 ms");
    }

    #[test]
    fn format_duration_seconds_three_decimals() {
        assert_eq!(format_duration_ns(1_547_000_000), "1.547 s");
    }

    #[test]
    fn format_bytes_uses_binary_prefixes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1 MiB");
        assert_eq!(format_bytes(612 * 1024 * 1024), "612 MiB");
        assert_eq!(format_bytes(2 * 1024 * 1024 * 1024), "2.00 GiB");
    }
}
