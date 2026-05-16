//! inferscope: a profiling and observability tool for LLM inference engines.
//!
//! Entry point and runtime orchestration. The CLI argument schema
//! lives in the [`cli`] module; this file ties probe, sysmon, and
//! report together.

mod cli;

use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use tokio::sync::oneshot;

use is_probe::{config::ProbeConfig, runner::run as run_probe};
use is_report::{derive_resource, derive_timing, render_json, render_text, Report};
use is_sysmon::{config::SysmonConfig, sampler::sample_during};

use crate::cli::Args;

fn main() -> ExitCode {
    let args = Args::parse();

    // Build a multi-threaded tokio runtime so the probe (network
    // I/O) and the sysmon (filesystem I/O on /proc) can make real
    // progress in parallel rather than time-sharing one thread.
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

/// Runs one probe (plus optional sysmon), builds the report, and
/// writes it to stdout. Returns a friendly error string on failure.
async fn orchestrate(args: Args) -> Result<(), String> {
    // The single reference instant shared between the probe and
    // the sysmon, per ADR-003. Captured before either task starts
    // so both produce elapsed_ns from the same origin.
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
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let task = tokio::spawn(sample_during(cfg, start, cancel_rx));
        (task, cancel_tx)
    });

    // Run the probe to completion. The probe owns the wall clock
    // of the profiling run; sysmon is purely along for the ride.
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

    // Surface the probe error only after sysmon has been cleaned
    // up. Doing it earlier would leak the sampling task.
    let request_timing = probe_result?;

    let timing = derive_timing(&request_timing);
    let resource = match resource_timeline.as_ref() {
        Some(tl) => derive_resource(tl).map_err(|e| format!("report derivation failed: {e}"))?,
        None => None,
    };

    let report = Report {
        request_timing,
        resource_timeline,
        timing,
        resource,
    };

    if args.json {
        let json = render_json(&report).map_err(|e| format!("json render failed: {e}"))?;
        println!("{json}");
    } else {
        print!("{}", render_text(&report));
    }

    Ok(())
}
