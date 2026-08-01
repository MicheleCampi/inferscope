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

use tokio::sync::oneshot;

use is_metrics::{scrape_during, scrape_phase_during, Engine, MetricsConfig};

use is_probe::{config::ProbeConfig, runner::run as run_probe};
use is_report::{
    derive_cost, derive_efficiency, derive_gpu, derive_kvcache, derive_phase_energy,
    derive_resource, derive_timing, render_json, render_resource_json, render_text, CostBasis,
    Report, ResourceReport, TrajectoryCost,
};
use is_sysmon::{config::SysmonConfig, sampler::sample_during};

#[cfg(feature = "gpu-nvidia")]
use is_core::GpuTimeline;
#[cfg(feature = "gpu-nvidia")]
use is_sysmon::{sample_gpu_during, GpuSampler};

use crate::cli::{Args, Commands};

fn main() -> ExitCode {
    let args = match cli::parse_checked() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("inferscope: {message}");
            return ExitCode::from(2);
        }
    };

    // Modes that are not a run branch here, before the engine
    // vocabulary is resolved and before a runtime is built: deriving
    // cost reads a file and multiplies, it needs neither. Branching
    // first is also what keeps the run flags inert, since
    // `subcommand_negates_reqs` lets a subcommand coexist with them
    // on the command line without making them meaningful.
    if let Some(command) = args.command.as_ref() {
        return run_command(command);
    }
    // Resolve the metric vocabulary before anything else (ADR-014 D6).
    // clap guarantees --engine is present whenever --metrics-endpoint
    // is, so a None here means no scrape will be attempted.
    let engine = match args.engine() {
        Ok(engine) => engine,
        Err(message) => {
            eprintln!("inferscope: {message}");
            return ExitCode::FAILURE;
        }
    };

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

    match runtime.block_on(orchestrate(args, engine)) {
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
async fn orchestrate(args: Args, engine: Option<Engine>) -> Result<(), String> {
    // The single reference instant shared between the probe, the
    // /proc sampler, and the GPU sampler, per ADR-003 and ADR-005.
    // Captured before any task starts so all three produce
    // elapsed_ns from the same origin.
    let start = Instant::now();
    // Wall-clock anchor for the ADR-003 reference instant (ADR-013):
    // read once, here, in the same statement sequence as the monotonic
    // reference. One wall-clock read means NTP adjustments during the
    // run cannot bend the timeline; the anchoring error is a per-run
    // constant on the order of microseconds.
    let reference_instant_unix_ns: Option<u64> = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok());

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
        return run_sample_only(&args, engine, start, reference_instant_unix_ns).await;
    }

    // In the normal path, clap guarantees endpoint, model, and prompt
    // are present via `required_unless_present = "sample_only"`. Two
    // paths relax that requirement and both return before this point:
    // sample-only just above, and any subcommand in `main` before the
    // runtime is built (`subcommand_negates_reqs`). So these unwraps
    // cannot fire.
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
    let metrics_handle = match (
        args.metrics_endpoint.as_deref(),
        args.model.as_deref(),
        engine,
    ) {
        (Some(endpoint), Some(model), Some(engine)) => {
            // Both scrapes hit the same /metrics endpoint over the
            // same run window, sharing `start` and each its own cancel
            // (ADR-012). They are separate loops, not one GET split two
            // ways: KV hit-rate and phase energy are independent
            // first/last reductions, and keeping them separate leaves
            // the ADR-011 KV path untouched.
            let kv_cfg = MetricsConfig::with_period(endpoint, model, engine, args.metrics_period());
            let phase_cfg =
                MetricsConfig::with_period(endpoint, model, engine, args.metrics_period());
            let (kv_cancel, kv_rx) = oneshot::channel();
            let (phase_cancel, phase_rx) = oneshot::channel();
            let kv_task = tokio::spawn(scrape_during(kv_cfg, start, kv_rx));
            let phase_task = tokio::spawn(scrape_phase_during(phase_cfg, start, phase_rx));
            Some((kv_task, kv_cancel, phase_task, phase_cancel))
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
    let (kvcache_timeline, phase_timeline) =
        if let Some((kv_task, kv_cancel, phase_task, phase_cancel)) = metrics_handle {
            let _ = kv_cancel.send(());
            let _ = phase_cancel.send(());
            let kv = match kv_task.await {
                Ok(timeline) => Some(timeline),
                Err(e) => {
                    eprintln!("inferscope: warning: metrics scrape task ended abnormally: {e}");
                    None
                }
            };
            let phase = match phase_task.await {
                Ok(timeline) => Some(timeline),
                Err(e) => {
                    eprintln!("inferscope: warning: phase scrape task ended abnormally: {e}");
                    None
                }
            };
            (kv, phase)
        } else {
            (None, None)
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
    // Per-phase energy attribution apportions the aggregate GPU energy
    // figure across prefill and decode by two bases (ADR-012). `None`
    // propagates when no phase timeline was scraped, a counter
    // regressed, no energy existed to apportion, or a basis delta was
    // zero.
    let phase_energy = phase_timeline.as_ref().and_then(|tl| {
        let (mj, source) = gpu
            .as_ref()
            .map_or((None, None), |g| (g.energy_millijoules, g.energy_source));
        derive_phase_energy(tl, mj, source)
    });

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
        phase_timeline,
        phase_energy,
        trajectory: None,
        reference_instant_unix_ns,
        schema_version: Some(is_report::REPORT_SCHEMA_VERSION),
    };
    // Trajectory-level attribution (ADR-013): offline join of the
    // driver's step boundaries against the report's timelines.
    // Withholding conditions inside `derive_trajectory` propagate as
    // an absent section, not errors.
    let report = match args.steps_file.as_deref() {
        Some(path) => {
            let steps = read_steps(path)?;
            let trajectory = is_report::derive_trajectory(&report, &steps);
            Report {
                trajectory,
                ..report
            }
        }
        None => report,
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
/// Reads and parses the driver step file (ADR-013). Structural
/// errors are fatal and name the offending line — the file is user
/// input, and a broken file is a broken input, not a withholding
/// condition.
fn read_steps(path: &std::path::Path) -> Result<Vec<is_report::StepRecord>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read steps file {}: {e}", path.display()))?;
    is_report::parse_steps(&content).map_err(|e| format!("steps file {}: {e}", path.display()))
}

async fn run_sample_only(
    args: &Args,
    engine: Option<Engine>,
    start: Instant,
    reference_instant_unix_ns: Option<u64>,
) -> Result<(), String> {
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

    // Spawn the per-phase metrics scrape if --metrics-endpoint and
    // --model were supplied (ADR-012). Not feature-gated: the scrape
    // is an HTTP read of the engine's Prometheus endpoint, not NVML.
    // It shares `start` with the samplers and is cancelled by the
    // same timer below (ADR-003).
    let phase_handle = match (
        args.metrics_endpoint.as_deref(),
        args.model.as_deref(),
        engine,
    ) {
        (Some(endpoint), Some(model), Some(engine)) => {
            let cfg = MetricsConfig::with_period(endpoint, model, engine, args.metrics_period());
            let (cancel_tx, cancel_rx) = oneshot::channel();
            let task = tokio::spawn(scrape_phase_during(cfg, start, cancel_rx));
            Some((task, cancel_tx))
        }
        _ => None,
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
    let phase_timeline = if let Some((task, cancel_tx)) = phase_handle {
        let _ = cancel_tx.send(());
        match task.await {
            Ok(tl) => Some(tl),
            Err(e) => {
                eprintln!("inferscope: warning: phase scrape task ended abnormally: {e}");
                None
            }
        }
    } else {
        None
    };

    let resource = match resource_timeline.as_ref() {
        Some(tl) => derive_resource(tl).map_err(|e| format!("report derivation failed: {e}"))?,
        None => None,
    };
    let gpu = gpu_timeline.as_ref().and_then(derive_gpu);

    // Apportion the sampled device energy across prefill and decode
    // (ADR-012), using the same aggregate figure as efficiency.
    let phase_energy = phase_timeline.as_ref().and_then(|tl| {
        let (mj, source) = gpu
            .as_ref()
            .map_or((None, None), |g| (g.energy_millijoules, g.energy_source));
        derive_phase_energy(tl, mj, source)
    });
    // Trajectory-level attribution (ADR-013) in attach mode: the
    // sample-only path holds the raw timelines directly. No KV-cache
    // scrape exists in this path, so that slice is absent by
    // construction.
    let trajectory = match args.steps_file.as_deref() {
        Some(path) => {
            let steps = read_steps(path)?;
            is_report::derive_trajectory_from_timelines(
                reference_instant_unix_ns,
                gpu_timeline.as_ref(),
                None,
                phase_timeline.as_ref(),
                &steps,
            )
        }
        None => None,
    };
    let report = ResourceReport {
        reference_instant_unix_ns,
        pid,
        include_descendants: args.include_descendants,
        sample_period_ms: args.sample_period_ms,
        duration_secs,
        resource,
        gpu,
        phase_timeline,
        phase_energy,
        trajectory,
        schema_version: Some(is_report::REPORT_SCHEMA_VERSION),
    };

    // Sample-only always emits JSON: it is meant for machine consumption
    // by the analysis pipeline (the A/B experiment), not human reading.
    let json = render_resource_json(&report).map_err(|e| format!("json render failed: {e}"))?;
    println!("{json}");
    Ok(())
}

/// Runs a mode that is not a profiling run.
///
/// Synchronous by construction: nothing here touches the network,
/// the clock or /proc.
fn run_command(command: &Commands) -> ExitCode {
    match command {
        Commands::Cost {
            report,
            usd_per_hour,
            usd_per_kwh,
        } => match derive_cost_from_report(report, *usd_per_hour, *usd_per_kwh) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("inferscope: {message}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Validates a declared rate.
///
/// clap parses the float; it does not constrain its domain. A
/// negative or non-finite rate would multiply through `derive_cost`
/// and produce a figure that looks like a price.
fn checked_rate(value: f64, flag: &str) -> Result<f64, String> {
    if !value.is_finite() {
        return Err(format!("{flag} must be a finite number, got {value}"));
    }
    if value <= 0.0 {
        return Err(format!("{flag} must be greater than zero, got {value}"));
    }
    Ok(value)
}

/// Reads an archived report, derives cost at the declared rate, and
/// renders it.
///
/// The report is never written back: cost lives outside the
/// serialized artifact (ADR-015 D1).
fn derive_cost_from_report(
    path: &std::path::Path,
    usd_per_hour: Option<f64>,
    usd_per_kwh: Option<f64>,
) -> Result<String, String> {
    // clap guarantees exactly one is present: the two conflict, and
    // one is required unless the other is given.
    let basis = match (usd_per_hour, usd_per_kwh) {
        (Some(rate), None) => CostBasis::Occupancy {
            usd_per_hour: checked_rate(rate, "--usd-per-hour")?,
        },
        (None, Some(rate)) => CostBasis::Energy {
            usd_per_kwh: checked_rate(rate, "--usd-per-kwh")?,
        },
        _ => return Err("exactly one rate must be given".to_string()),
    };

    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read report {}: {e}", path.display()))?;
    let report: ResourceReport = serde_json::from_str(&raw)
        .map_err(|e| format!("cannot parse report {}: {e}", path.display()))?;

    // Three outcomes, kept distinct. A report with no trajectory
    // section and a trajectory that cannot be priced are different
    // facts, and neither is a cost of zero.
    let Some(trajectory) = report.trajectory.as_ref() else {
        return Err(format!(
            "report {} carries no trajectory section: cost is defined \
             per trajectory step (ADR-015), so there is nothing to price. \
             Re-run with --steps-file to produce one.",
            path.display()
        ));
    };
    let Some(cost) = derive_cost(trajectory, basis) else {
        return Err(format!(
            "report {} carries a trajectory but no quantity this basis \
             can price: the run duration or the measured energy is zero. \
             A report written before ADR-015 deserializes to a zero run \
             duration and is withheld rather than priced at zero.",
            path.display()
        ));
    };
    Ok(render_cost(&cost))
}

/// Renders a derived cost as text.
///
/// The basis and its rate appear in the header and again on the
/// whole-run line, which is the line most likely to be quoted on its
/// own. A dollar figure without the rate that produced it is not a
/// measurement of anything.
fn render_cost(cost: &TrajectoryCost) -> String {
    let (basis_label, rate_label) = match cost.basis {
        CostBasis::Occupancy { usd_per_hour } => (
            "occupancy (node rented by wall-clock time; energy already priced in)",
            format!("${usd_per_hour:.4}/hour"),
        ),
        CostBasis::Energy { usd_per_kwh } => (
            "energy (hardware owned; electricity metered separately)",
            format!("${usd_per_kwh:.4}/kWh"),
        ),
    };

    let mut out = String::new();
    out.push_str("=== cost attribution (derived, not measured) ===\n");
    out.push_str(&format!("basis:            {basis_label}\n"));
    out.push_str(&format!("declared rate:    {rate_label}\n"));
    out.push('\n');
    out.push_str(&format!(
        "run window:       ${:.6} at {rate_label}\n",
        cost.run_usd
    ));
    out.push_str(&format!("  attributed:     ${:.6}\n", cost.attributed_usd));
    out.push_str(&format!(
        "  unattributed:   ${:.6} ({:.1}% of run)\n",
        cost.unattributed_usd,
        if cost.run_usd > 0.0 {
            100.0 * cost.unattributed_usd / cost.run_usd
        } else {
            0.0
        }
    ));
    match cost.trajectory_usd_per_million_tokens {
        Some(v) => out.push_str(&format!(
            "per M gen tokens: ${v:.4} at {rate_label} (over the whole run window)\n"
        )),
        None => out.push_str("per M gen tokens: withheld (no generation tokens in this run)\n"),
    }

    out.push_str(&format!(
        "\nper-step ({} steps, at {rate_label}):\n",
        cost.steps.len()
    ));
    for step in &cost.steps {
        let per_m = match step.usd_per_million_tokens {
            Some(v) => format!("${v:.4}/Mtok"),
            None => "-".to_string(),
        };
        // Width applies to the rendered name, not to the nested
        // format!: `{:<6}` on a format! argument pads nothing.
        let kind = format!("{:?}", step.kind).to_lowercase();
        out.push_str(&format!(
            "  step {:>4}  {kind:<8}  ${:.6}  {per_m}\n",
            step.step_id, step.usd
        ));
    }
    out.push_str(
        "\nSingle-tenant profiling run. The run window is not the invoice:\n\
         provisioning, model load and post-run idle are billed and are not\n\
         in it (ADR-015 D7).\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use is_report::{StepKind, StepMetrics, TrajectoryMetrics};

    /// A trajectory with one LLM step and one tool step, priced by
    /// occupancy. Figures are shaped to be checkable by hand: a run
    /// window of 3600s at $1/hour is exactly $1.
    fn trajectory() -> TrajectoryMetrics {
        TrajectoryMetrics {
            steps: vec![
                StepMetrics {
                    step_id: 1,
                    kind: StepKind::LlmCall,
                    start_elapsed_ns: 0,
                    end_elapsed_ns: 1_800_000_000_000,
                    samples_in_window: 10,
                    energy_mj: 500_000,
                    generation_tokens_delta: Some(1_000),
                    prompt_tokens_delta: Some(50),
                    cache_hits_delta: None,
                    cache_queries_delta: None,
                    // 1_000 tokens over 500_000 mJ = 500 J.
                    tokens_per_joule: Some(2.0),
                    cache_hit_rate: None,
                },
                StepMetrics {
                    step_id: 2,
                    kind: StepKind::Tool,
                    start_elapsed_ns: 1_800_000_000_000,
                    end_elapsed_ns: 2_700_000_000_000,
                    samples_in_window: 5,
                    energy_mj: 100_000,
                    generation_tokens_delta: None,
                    prompt_tokens_delta: None,
                    cache_hits_delta: None,
                    cache_queries_delta: None,
                    // A tool step produces no tokens: absence, not zero.
                    tokens_per_joule: None,
                    cache_hit_rate: None,
                },
            ],
            total_energy_mj: 700_000,
            total_generation_tokens: 1_000,
            trajectory_tokens_per_joule: Some(1.428_571_4),
            llm_energy_mj: 500_000,
            tool_energy_mj: 100_000,
            unattributed_energy_mj: 100_000,
            run_duration_ns: 3_600_000_000_000,
            unattributed_duration_ns: 900_000_000_000,
            dropped_steps: vec![],
        }
    }

    fn report_with(trajectory: Option<TrajectoryMetrics>) -> ResourceReport {
        ResourceReport {
            reference_instant_unix_ns: None,
            pid: 1,
            include_descendants: false,
            sample_period_ms: 100,
            duration_secs: 3600,
            resource: None,
            gpu: None,
            phase_timeline: None,
            phase_energy: None,
            trajectory,
            schema_version: None,
        }
    }

    fn write(report: &ResourceReport) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        std::fs::write(&path, render_resource_json(report).unwrap()).unwrap();
        (dir, path)
    }

    #[test]
    fn cost_round_trips_through_a_serialized_report() {
        let (_dir, path) = write(&report_with(Some(trajectory())));
        let out = derive_cost_from_report(&path, Some(1.0), None).unwrap();

        // The whole point of the subcommand: the rate travels with
        // the figure, on the line most likely to be quoted alone.
        assert!(out.contains("$1.0000/hour"), "{out}");
        assert!(
            out.contains("run window:       $1.000000 at $1.0000/hour"),
            "{out}"
        );
        // 900s of 3600s is outside every kept step window.
        assert!(out.contains("$0.250000 (25.0% of run)"), "{out}");
        assert!(out.contains("derived, not measured"), "{out}");
        assert!(out.contains("step    1  llmcall"), "{out}");
        assert!(out.contains("step    2  tool"), "{out}");
    }

    #[test]
    fn cost_is_withheld_for_a_report_predating_adr_015() {
        // `run_duration_ns` has serde(default), so a report written
        // before ADR-015 deserializes to zero. Zero duration is
        // absence of the measurement, not a run that cost nothing.
        let mut pre = trajectory();
        pre.run_duration_ns = 0;
        let (_dir, path) = write(&report_with(Some(pre)));
        let err = derive_cost_from_report(&path, Some(1.0), None).unwrap_err();
        assert!(err.contains("withheld rather than priced at zero"), "{err}");
    }

    #[test]
    fn cost_names_the_missing_section_when_there_is_no_trajectory() {
        let (_dir, path) = write(&report_with(None));
        let err = derive_cost_from_report(&path, Some(1.0), None).unwrap_err();
        assert!(err.contains("no trajectory section"), "{err}");
        assert!(err.contains("--steps-file"), "{err}");
    }

    #[test]
    fn a_rate_outside_its_domain_is_rejected_before_it_multiplies() {
        let (_dir, path) = write(&report_with(Some(trajectory())));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = derive_cost_from_report(&path, Some(bad), None).unwrap_err();
            assert!(err.contains("--usd-per-hour"), "{bad}: {err}");
        }
    }
}
