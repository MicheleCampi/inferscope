//! inferscope: a profiling and observability tool for LLM inference engines.
//!
//! Entry point and runtime orchestration. The CLI argument schema
//! lives in the [`cli`] module; this file ties probe, sysmon, and
//! report together. When built with the `gpu-nvidia` feature, the
//! orchestrator additionally spawns a GPU sampler task in parallel
//! with the probe and the /proc sampler (per ADR-005).

mod cli;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use tokio::sync::oneshot;

use is_metrics::{scrape_during, MetricsConfig};
use is_probe::{config::ProbeConfig, runner::run as run_probe};
use is_report::{
    derive_efficiency, derive_gpu, derive_kvcache, derive_resource, derive_timing, render_json,
    render_resource_json, render_text, Report, ResourceReport,
};
use is_sysmon::{config::SysmonConfig, sampler::sample_during};

#[cfg(feature = "gpu-nvidia")]
use is_core::GpuTimeline;
#[cfg(feature = "gpu-nvidia")]
use is_sysmon::{sample_gpu_during, GpuSampler};

use crate::cli::Args;

fn main() -> ExitCode {
    let args = Args::parse();

    // Build a multi-threaded tokio runtime so the probe (network
    // I/O), the sysmon (filesystem I/O on /proc), and the optional
    // GPU sampler (NVML calls) can make real progress in parallel
    // rather than time-sharing one thread.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("inferscope: failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(orchestrate(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("inferscope: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one probe (plus optional sysmon and GPU sampler), builds
/// the report, and writes it to stdout. Returns a friendly error
/// string on failure.
async fn orchestrate(args: Args) -> Result<(), String> {
    // The single reference instant shared between the probe, the
    // /proc sampler, and the GPU sampler, per ADR-003 and ADR-005.
    // Captured before any task starts so all three produce
    // elapsed_ns from the same origin.
    let start = Instant::now();

    // Validate the PID early: if the user passed --pid pointing
    // to a process that does not exist, fail before doing any
    // network I/O. /proc/<pid> existence is the canonical Linux
    // check; we accept a small TOCTOU window (process can die
    // between this check and the first sample) — sysmon already
    // handles a vanishing process gracefully during sampling.
    if let Some(pid) = args.pid {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return Err(format!(
                "process with pid {pid} does not exist (no /proc/{pid})"
            ));
        }
    }

    // Sample-only mode returns here: no probe, just resource sampling
    // for a fixed duration while an external load generator drives traffic.
    if args.sample_only {
        return run_sample_only(&args, start).await;
    }

    // In the normal (non sample-only) path, clap guarantees endpoint,
    // model, and prompt are present via `required_unless_present =
    // "sample_only"`. The sample-only path returns earlier (above) and
    // never reaches here, so these unwraps cannot fire.
    let probe_cfg = ProbeConfig::new(
        args.endpoint
            .clone()
            .expect("endpoint is required unless --sample-only"),
        args.model
            .clone()
            .expect("model is required unless --sample-only"),
        args.prompt
            .clone()
            .expect("prompt is required unless --sample-only"),
        args.max_tokens,
    );

    // Spawn sysmon if a PID was supplied. The cancellation
    // oneshot lets us stop the sampler the moment the probe
    // finishes — no extra samples taken after the timing run
    // is over.
    let sysmon_handle = args.pid.map(|pid| {
        let cfg = SysmonConfig::with_period(pid, args.sample_period());
        let cfg = if args.include_descendants {
            cfg.with_descendants()
        } else {
            cfg
        };
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let task = tokio::spawn(sample_during(cfg, start, cancel_rx));
        (task, cancel_tx)
    });

    // Spawn GPU sampler if --gpu was supplied and the feature is
    // compiled in. Failure to initialise NVML is non-fatal:
    // we emit a warning and continue without GPU sampling, per
    // ADR-005.
    #[cfg(feature = "gpu-nvidia")]
    let gpu_handle = if args.gpu {
        match GpuSampler::new() {
            Ok(sampler) => {
                let cfg = SysmonConfig::with_period(0, args.sample_period());
                let (cancel_tx, cancel_rx) = oneshot::channel();
                let task = tokio::spawn(sample_gpu_during(sampler, cfg, start, cancel_rx));
                Some((task, cancel_tx))
            }
            Err(e) => {
                eprintln!("inferscope: warning: GPU sampling requested but unavailable: {e}");
                None
            }
        }
    } else {
        None
    };

    // Spawn the metrics scrape task if --metrics-endpoint was supplied
    // (ADR-011). Unlike the GPU sampler this is not feature-gated: the
    // crate is always compiled and activates on the flag. The scrape
    // shares `start` with the other samplers so its samples sit on the
    // same elapsed_ns clock (ADR-003). The --model value selects the
    // model_name label series.
    let metrics_handle = match (args.metrics_endpoint.as_deref(), args.model.as_deref()) {
        (Some(endpoint), Some(model)) => {
            let cfg = MetricsConfig::with_period(endpoint, model, args.metrics_period());
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let task = tokio::spawn(scrape_during(cfg, start, cancel_rx));
            Some((task, cancel_tx))
        }
        _ => None,
    };
    // Run the probe to completion. The probe owns the wall clock
    // of the profiling run; sysmon and GPU sampler are along for
    // the ride.
    let probe_result = run_probe(&probe_cfg)
        .await
        .map_err(|e| format!("probe failed: {e}"));

    // Stop sysmon (if it was running) and collect whatever
    // samples it gathered. We do this regardless of whether the
    // probe succeeded, so even on a probe failure we still close
    // the sampling task cleanly.
    let resource_timeline = if let Some((task, cancel_tx)) = sysmon_handle {
        let _ = cancel_tx.send(());
        match task.await {
            Ok(timeline) => Some(timeline),
            Err(e) => {
                // A panicked sampler task is not fatal for the
                // overall run — we surface a warning and report
                // whatever the probe captured.
                eprintln!("inferscope: warning: sysmon task ended abnormally: {e}");
                None
            }
        }
    } else {
        None
    };

    // Stop GPU sampler with the same shape as the /proc sampler.
    #[cfg(feature = "gpu-nvidia")]
    let gpu_timeline: Option<GpuTimeline> = if let Some((task, cancel_tx)) = gpu_handle {
        let _ = cancel_tx.send(());
        match task.await {
            Ok(timeline) => Some(timeline),
            Err(e) => {
                eprintln!("inferscope: warning: GPU sampler task ended abnormally: {e}");
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(feature = "gpu-nvidia"))]
    let gpu_timeline = None;

    // Surface the probe error only after both samplers have been
    // cleaned up. Doing it earlier would leak the sampling tasks.
    let request_timing = probe_result?;
    // Stop the metrics scrape (if running) and collect its timeline,
    // same shape as the other samplers. A panicked task is non-fatal:
    // warn and report whatever the probe captured (ADR-011).
    let kvcache_timeline = if let Some((task, cancel_tx)) = metrics_handle {
        let _ = cancel_tx.send(());
        match task.await {
            Ok(timeline) => Some(timeline),
            Err(e) => {
                eprintln!("inferscope: warning: metrics scrape task ended abnormally: {e}");
                None
            }
        }
    } else {
        None
    };

    // Sanity check: warn if the monitored PID looks like a
    // wrapper rather than the actual workload. If every sample
    // we collected has RSS < 10 MiB AND exactly 1 thread AND
    // zero CPU jiffies, the user almost certainly passed the
    // PID of a transient shell (e.g. $! after bash redirection)
    // rather than the long-lived worker. We do not fail — just
    // surface the suspicion so the result is interpreted
    // correctly.
    if let Some(tl) = resource_timeline.as_ref() {
        let suspicious = !tl.samples.is_empty()
            && tl.samples.iter().all(|s| s.rss_bytes < 10 * 1024 * 1024)
            && tl.samples.iter().all(|s| s.thread_count == 1)
            && tl
                .samples
                .iter()
                .all(|s| s.cpu_user_jiffies == 0 && s.cpu_system_jiffies == 0);
        if suspicious {
            eprintln!(
                "inferscope: warning: monitored PID looks idle across all {} samples \
                 (RSS < 10 MiB, 1 thread, 0 CPU jiffies). The --pid argument may point \
                 to a wrapper shell rather than the actual workload. \
                 Verify with: cat /proc/<pid>/status | grep -E 'VmRSS|Threads'.",
                tl.samples.len()
            );
        }
    }

    let timing = derive_timing(&request_timing);
    let resource = match resource_timeline.as_ref() {
        Some(tl) => derive_resource(tl).map_err(|e| format!("report derivation failed: {e}"))?,
        None => None,
    };

    let gpu = gpu_timeline.as_ref().and_then(derive_gpu);

    // Efficiency derives from the aggregate energy figure on the
    // GPU metrics and the token count (ADR-010). `None` propagates
    // when energy was unmeasurable or no tokens were produced.
    let efficiency = gpu
        .as_ref()
        .and_then(|g| derive_efficiency(g.energy_millijoules, g.energy_source, timing.token_count));
    // KV-cache hit rate derives from the scraped timeline: the window
    // delta of hits over queries (ADR-011). `None` propagates when no
    // endpoint was scraped or the window was invalid (counter reset,
    // or zero queries).
    let kvcache = kvcache_timeline.as_ref().and_then(derive_kvcache);

    let report = Report {
        request_timing,
        resource_timeline,
        gpu_timeline,
        timing,
        resource,
        gpu,
        efficiency,
        kvcache_timeline,
        kvcache,
    };

    if args.json {
        let json = render_json(&report).map_err(|e| format!("json render failed: {e}"))?;
        println!("{json}");
    } else {
        print!("{}", render_text(&report));
    }

    // Optional OpenTelemetry export. Failure is logged but does not
    // change the exit code: observability is secondary to the
    // profiling result. See ADR-008.
    #[cfg(feature = "otel-export")]
    if let Some(endpoint) = args.otel_endpoint.as_deref() {
        if let Err(e) = is_report::export_to_otel(&report, endpoint) {
            eprintln!("inferscope: OpenTelemetry export failed: {e}");
        }
    }

    Ok(())
}

/// Runs sample-only mode: attaches to an already-running PID and
/// samples its resource usage (and optionally per-device GPU usage)
/// for a fixed duration, WITHOUT issuing any inference request.
///
/// Cancellation is driven by a timer of `duration_secs` rather than
/// by probe completion (there is no probe here). Emits a
/// resource-only report. See ADR-009.
async fn run_sample_only(args: &Args, start: Instant) -> Result<(), String> {
    let pid = args
        .pid
        .ok_or_else(|| "--sample-only requires --pid".to_string())?;
    let duration_secs = args
        .duration_secs
        .ok_or_else(|| "--sample-only requires --duration-secs".to_string())?;
    let duration = Duration::from_secs(duration_secs);

    // Spawn the /proc sampler, cancelled by a timer below.
    let cfg = SysmonConfig::with_period(pid, args.sample_period());
    let cfg = if args.include_descendants {
        cfg.with_descendants()
    } else {
        cfg
    };
    let (sysmon_cancel_tx, sysmon_cancel_rx) = oneshot::channel();
    let sysmon_task = tokio::spawn(sample_during(cfg, start, sysmon_cancel_rx));

    // Spawn the GPU sampler if requested and the feature is present.
    #[cfg(feature = "gpu-nvidia")]
    let gpu_handle = if args.gpu {
        match GpuSampler::new() {
            Ok(sampler) => {
                let gcfg = SysmonConfig::with_period(0, args.sample_period());
                let (cancel_tx, cancel_rx) = oneshot::channel();
                let task = tokio::spawn(sample_gpu_during(sampler, gcfg, start, cancel_rx));
                Some((task, cancel_tx))
            }
            Err(e) => {
                eprintln!("inferscope: warning: GPU sampling requested but unavailable: {e}");
                None
            }
        }
    } else {
        None
    };

    // Sample for the requested duration, then cancel.
    tokio::time::sleep(duration).await;

    let _ = sysmon_cancel_tx.send(());
    let resource_timeline = match sysmon_task.await {
        Ok(tl) => Some(tl),
        Err(e) => {
            eprintln!("inferscope: warning: sysmon task ended abnormally: {e}");
            None
        }
    };

    #[cfg(feature = "gpu-nvidia")]
    let gpu_timeline = if let Some((task, cancel_tx)) = gpu_handle {
        let _ = cancel_tx.send(());
        match task.await {
            Ok(tl) => Some(tl),
            Err(e) => {
                eprintln!("inferscope: warning: GPU sampler task ended abnormally: {e}");
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(feature = "gpu-nvidia"))]
    let gpu_timeline: Option<is_core::GpuTimeline> = None;

    let resource = match resource_timeline.as_ref() {
        Some(tl) => derive_resource(tl).map_err(|e| format!("report derivation failed: {e}"))?,
        None => None,
    };
    let gpu = gpu_timeline.as_ref().and_then(derive_gpu);

    let report = ResourceReport {
        pid,
        include_descendants: args.include_descendants,
        sample_period_ms: args.sample_period_ms,
        duration_secs,
        resource,
        gpu,
    };

    // Sample-only always emits JSON: it is meant for machine consumption
    // by the analysis pipeline (the A/B experiment), not human reading.
    let json = render_resource_json(&report).map_err(|e| format!("json render failed: {e}"))?;
    println!("{json}");
    Ok(())
}
