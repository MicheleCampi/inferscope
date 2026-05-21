//! inferscope: a profiling and observability tool for LLM inference engines.
//!
//! Entry point and runtime orchestration. The CLI argument schema
//! lives in the [`cli`] module; this file ties probe, sysmon, and
//! report together. When built with the `gpu-nvidia` feature, the
//! orchestrator additionally spawns a GPU sampler task in parallel
//! with the probe and the /proc sampler (per ADR-005).

mod cli;

use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use tokio::sync::oneshot;

use is_probe::{config::ProbeConfig, runner::run as run_probe};
use is_report::{derive_gpu, derive_resource, derive_timing, render_json, render_text, Report};
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

    let probe_cfg = ProbeConfig::new(
        args.endpoint.clone(),
        args.model.clone(),
        args.prompt.clone(),
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
                eprintln!(
                    "inferscope: warning: GPU sampling requested but unavailable: {e}"
                );
                None
            }
        }
    } else {
        None
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
            && tl.samples.iter().all(|s| s.cpu_user_jiffies == 0 && s.cpu_system_jiffies == 0);
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

    let report = Report {
        request_timing,
        resource_timeline,
        gpu_timeline,
        timing,
        resource,
        gpu,
    };

    if args.json {
        let json = render_json(&report).map_err(|e| format!("json render failed: {e}"))?;
        println!("{json}");
    } else {
        print!("{}", render_text(&report));
    }

    Ok(())
}
