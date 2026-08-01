//! Command-line argument parsing.
//!
//! The CLI surface is intentionally small. The flags map directly
//! to fields of `ProbeConfig` and `SysmonConfig`, with one
//! orchestrator-level flag (`--pid`) and one output flag
//! (`--json`). When the `gpu-nvidia` Cargo feature is enabled,
//! an additional `--gpu` flag becomes available.

use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use is_metrics::Engine;

/// Profile an OpenAI-compatible LLM inference engine.
///
/// inferscope sends one streamed chat completions request to the
/// given endpoint, captures per-token timing, and — if a PID is
/// supplied — correlates that timing with the engine process's
/// resource footprint sampled from /proc. The result is rendered
/// as either a human-readable ASCII report (default) or a JSON
/// document carrying both the raw signals and derived metrics.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
// A subcommand is a different mode of operation, not a variant of a
// run: when one is present the run flags do not apply and must stop
// being required. Verified against clap 4.5.20: without this, every
// subcommand invocation fails on the run-level requirements.
#[command(subcommand_negates_reqs = true)]
pub struct Args {
    /// Base URL of the engine's OpenAI-compatible API.
    ///
    /// The chat completions path is appended automatically.
    /// Example: `http://localhost:8080`.
    #[arg(long, required_unless_present = "sample_only")]
    pub endpoint: Option<String>,

    /// Model identifier as the engine expects it.
    #[arg(long, required_unless_present = "sample_only")]
    pub model: Option<String>,

    /// Prompt sent as the single user message.
    #[arg(long, required_unless_present = "sample_only")]
    pub prompt: Option<String>,

    /// Maximum number of tokens to generate. Bounds the probe run.
    #[arg(long, default_value_t = 128)]
    pub max_tokens: u32,

    /// PID of the engine process to monitor for resource usage.
    ///
    /// When omitted, the report contains only timing data and no
    /// resource section. When supplied, sysmon reads `/proc/<pid>`
    /// in parallel with the probe.
    #[arg(long)]
    pub pid: Option<u32>,

    /// Resource sampling period in milliseconds.
    ///
    /// Defaults to 50 ms per ADR-003. Ignored when `--pid` is
    /// not set.
    #[arg(long, default_value_t = 50)]
    pub sample_period_ms: u64,
    /// Prometheus `/metrics` endpoint to scrape for KV-cache hit rate
    /// (ADR-011), e.g. `http://127.0.0.1:18000/metrics`. When set, a
    /// scrape task runs in parallel with the probe and the report
    /// carries the window hit rate. When unset, no scrape happens and
    /// the KV-cache section is absent. The `--model` value selects the
    /// `model_name` label series.
    #[arg(long, requires = "engine")]
    pub metrics_endpoint: Option<String>,
    /// Metric vocabulary of the engine behind `--metrics-endpoint`
    /// (ADR-014 D6). Required whenever an endpoint is scraped, with
    /// no default: the two vocabularies name different series, and a
    /// body of the wrong one yields no series rather than a wrong
    /// number. Auto-detection from the body is deliberately not
    /// offered — under any tolerant parse it would write absence as
    /// zero.
    #[arg(long, value_enum)]
    pub engine: Option<EngineKind>,
    /// The SGLang server's `page_size`, as configured at engine start
    /// (ADR-014 D6). Required with `--engine sglang` and rejected with
    /// `--engine vllm`. It is not exposed on `/metrics`, so it cannot
    /// be derived from a scrape, and it selects the hit-rate
    /// accounting class: exact tokens at 1, page-aligned above.
    /// SGLang resolves it to 1 on non-HIP, non-MUSA platforms and to
    /// 64 on HIP with vectorized_5d and on MUSA, so there is no value
    /// inferscope could default to without asserting the caller's
    /// hardware.
    #[arg(long)]
    pub page_size: Option<u32>,
    /// Scrape period for `--metrics-endpoint`, in milliseconds.
    ///
    /// Defaults to 1000 ms — deliberately slower than the 50 ms
    /// resource-sampling cadence, since a `/metrics` scrape is an HTTP
    /// round-trip reading per-request application counters (ADR-011).
    #[arg(long, default_value_t = 1000)]
    pub metrics_period_ms: u64,
    /// Driver-side step file (JSONL, one step object per line) for
    /// trajectory-level attribution (ADR-013). When set, the report
    /// carries per-step energy and token figures joined offline on
    /// the wall-clock anchor. When unset, no trajectory section is
    /// derived. The file is read after the run completes; a
    /// structurally invalid file is a fatal error naming the line.
    #[arg(long)]
    pub steps_file: Option<std::path::PathBuf>,

    /// Aggregate the monitored PID with the resource usage of
    /// its direct children.
    ///
    /// Use this when the engine you point `--pid` at forks a
    /// worker that does the real inference work (typical of
    /// `llama-server` and similar) and the parent process
    /// itself reports near-zero RSS / CPU / threads. With this
    /// flag set, each sample sums the parent's `/proc/<pid>`
    /// metrics with those of every PID listed in
    /// `/proc/<pid>/task/<pid>/children`. Failing per-child
    /// reads are tolerated silently (a child may exit between
    /// discovery and sample). Per ADR-006.
    #[arg(long, default_value_t = false)]
    pub include_descendants: bool,

    /// Sample-only mode: attach to an already-running process and
    /// record its resource usage for a fixed duration WITHOUT
    /// sending any inference request.
    ///
    /// Use this to profile a server while an external load generator
    /// (e.g. AIPerf) drives the traffic. Requires `--pid` and
    /// `--duration-secs`. In this mode `--endpoint`, `--model`, and
    /// `--prompt` are not used and need not be supplied. The output
    /// is a resource-only report (no timing section). See ADR-009.
    #[arg(long, default_value_t = false)]
    pub sample_only: bool,

    /// Sampling duration in seconds for `--sample-only` mode.
    ///
    /// Required when `--sample-only` is set; ignored otherwise.
    #[arg(long, required_if_eq("sample_only", "true"))]
    pub duration_secs: Option<u64>,

    /// Sample GPU resources via NVML in parallel with the probe.
    ///
    /// When supplied, inferscope initialises NVML and samples
    /// every visible NVIDIA GPU at the same cadence as the /proc
    /// sampler. If NVML cannot be loaded (no driver), the run
    /// continues without GPU data and notes the absence in the
    /// report. Per ADR-005.
    ///
    /// Only available when built with `--features gpu-nvidia`.
    #[cfg(feature = "gpu-nvidia")]
    #[arg(long, default_value_t = false)]
    pub gpu: bool,

    /// Emit the report as JSON instead of plain text.
    ///
    /// The JSON output carries both raw signals and derived
    /// metrics per ADR-004.
    #[arg(long, default_value_t = false)]
    pub json: bool,

    /// OTLP/HTTP endpoint to which the report is exported as an
    /// OpenTelemetry trace after the run completes.
    ///
    /// When supplied, after the report is rendered to stdout
    /// inferscope opens a root span `inferscope.run` and emits
    /// it via OTLP/HTTP to the given endpoint. Token arrivals
    /// become span events; the derived aggregates become span
    /// attributes. See ADR-008.
    ///
    /// Pass the base URL of the OTLP receiver, not the full
    /// `/v1/traces` path. Example: `http://localhost:4318`. Also
    /// reads the standard `OTEL_EXPORTER_OTLP_ENDPOINT`
    /// environment variable if the flag is not supplied.
    ///
    /// Export failure does not change the inferscope exit code;
    /// a warning is printed to stderr and the run is treated as
    /// successful.
    ///
    /// Only available when built with `--features otel-export`.
    #[cfg(feature = "otel-export")]
    #[arg(long, env = "OTEL_EXPORTER_OTLP_ENDPOINT")]
    pub otel_endpoint: Option<String>,

    /// Optional mode that is not a profiling run.
    ///
    /// When present, every run flag above is inert: `main` branches
    /// here before resolving anything else.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// The metric vocabulary selected by `--engine` (ADR-014 D6).
///
/// Distinct from [`is_metrics::Engine`], which carries SGLang's
/// `page_size`: the flag names the vocabulary, and the value is
/// composed with `--page-size` by [`Args::engine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EngineKind {
    /// vLLM, exposing the `vllm:` vocabulary.
    Vllm,
    /// SGLang, exposing the `sglang:` vocabulary.
    Sglang,
}

impl Args {
    /// Resolves `--engine` and `--page-size` into an [`Engine`].
    ///
    /// Returns `Ok(None)` when no engine was declared, which clap
    /// permits only when no metrics endpoint was supplied. Both
    /// directions of the page-size rule are enforced here rather
    /// than declaratively: a page size supplied with vLLM is a
    /// caller error, not a value to absorb silently.
    pub fn engine(&self) -> Result<Option<Engine>, String> {
        match (self.engine, self.page_size) {
            (None, _) => Ok(None),
            (Some(EngineKind::Vllm), None) => Ok(Some(Engine::Vllm)),
            (Some(EngineKind::Vllm), Some(_)) => Err(
                "--page-size applies to --engine sglang only; vLLM's block size \
                 does not change the hit-rate accounting class"
                    .to_string(),
            ),
            (Some(EngineKind::Sglang), Some(page_size)) => Ok(Some(Engine::Sglang { page_size })),
            (Some(EngineKind::Sglang), None) => Err(
                "--engine sglang requires --page-size: it is not exposed on \
                 /metrics and it selects the hit-rate accounting class"
                    .to_string(),
            ),
        }
    }

    /// Returns the resource sampling period as a `Duration`.
    pub fn sample_period(&self) -> Duration {
        Duration::from_millis(self.sample_period_ms)
    }

    /// Returns the metrics scrape period as a `Duration`.
    pub fn metrics_period(&self) -> Duration {
        Duration::from_millis(self.metrics_period_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn args_definition_is_self_consistent() {
        // clap's own debug_assert pass — verifies the derive
        // generates a coherent command (no conflicting flag names,
        // no broken default expressions).
        Args::command().debug_assert();
    }

    #[test]
    fn parses_minimal_required_args() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "hello",
        ])
        .expect("minimal args should parse");
        assert_eq!(args.endpoint.as_deref(), Some("http://localhost:8080"));
        assert_eq!(args.model.as_deref(), Some("llama3"));
        assert_eq!(args.prompt.as_deref(), Some("hello"));
        assert!(!args.sample_only);
        // Defaults apply.
        assert_eq!(args.max_tokens, 128);
        assert_eq!(args.pid, None);
        assert_eq!(args.sample_period_ms, 50);
        assert!(!args.json);
    }

    #[test]
    fn parses_full_args() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "ping",
            "--max-tokens",
            "32",
            "--pid",
            "12345",
            "--sample-period-ms",
            "20",
            "--json",
        ])
        .expect("full args should parse");
        assert_eq!(args.max_tokens, 32);
        assert_eq!(args.pid, Some(12345));
        assert_eq!(args.sample_period_ms, 20);
        assert!(args.json);
        assert_eq!(args.sample_period(), Duration::from_millis(20));
    }

    #[test]
    fn missing_required_endpoint_fails() {
        let result = Args::try_parse_from(["inferscope", "--model", "llama3", "--prompt", "hi"]);
        assert!(result.is_err());
    }

    /// With the `gpu-nvidia` feature enabled, `--gpu` is a valid
    /// flag and defaults to false.
    #[cfg(feature = "gpu-nvidia")]
    #[test]
    fn gpu_flag_defaults_to_false_when_feature_enabled() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "hi",
        ])
        .expect("args should parse without --gpu");
        assert!(!args.gpu);
    }

    /// With the `gpu-nvidia` feature enabled, `--gpu` flips the
    /// flag to true.
    #[cfg(feature = "gpu-nvidia")]
    #[test]
    fn gpu_flag_parses_when_supplied() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "hi",
            "--gpu",
        ])
        .expect("args should parse with --gpu");
        assert!(args.gpu);
    }

    #[test]
    fn include_descendants_defaults_to_false() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "hi",
        ])
        .expect("args should parse without --include-descendants");
        assert!(!args.include_descendants);
    }

    #[test]
    fn include_descendants_parses_when_supplied() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "hi",
            "--include-descendants",
        ])
        .expect("args should parse with --include-descendants");
        assert!(args.include_descendants);
    }

    /// With the `otel-export` feature enabled, `--otel-endpoint`
    /// is a valid optional flag and defaults to `None`.
    #[cfg(feature = "otel-export")]
    #[test]
    fn otel_endpoint_defaults_to_none_when_feature_enabled() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "hi",
        ])
        .expect("args should parse without --otel-endpoint");
        assert_eq!(args.otel_endpoint, None);
    }

    /// With the `otel-export` feature enabled, `--otel-endpoint`
    /// accepts a URL.
    #[cfg(feature = "otel-export")]
    #[test]
    fn otel_endpoint_parses_when_supplied() {
        let args = Args::try_parse_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8080",
            "--model",
            "llama3",
            "--prompt",
            "hi",
            "--otel-endpoint",
            "http://collector:4318",
        ])
        .expect("args should parse with --otel-endpoint");
        assert_eq!(
            args.otel_endpoint,
            Some("http://collector:4318".to_string())
        );
    }
    #[test]
    fn sample_only_does_not_require_endpoint_model_prompt() {
        // In sample-only mode the probe is skipped, so endpoint/model/prompt
        // are not required. Only --pid and --duration-secs matter.
        let args = Args::try_parse_from([
            "inferscope",
            "--sample-only",
            "--pid",
            "4242",
            "--duration-secs",
            "30",
        ])
        .expect("sample-only with pid + duration should parse without endpoint");
        assert!(args.sample_only);
        assert_eq!(args.pid, Some(4242));
        assert_eq!(args.duration_secs, Some(30));
        assert_eq!(args.endpoint, None);
        assert_eq!(args.model, None);
        assert_eq!(args.prompt, None);
    }

    #[test]
    fn sample_only_accepts_metrics_endpoint_and_model() {
        // The CUDA-graphs experiment attaches via --sample-only and also
        // scrapes per-phase metrics (ADR-012). --model is not required in
        // sample-only mode but must remain permitted, so the phase scrape
        // can select its model_name label series.
        let args = Args::try_parse_from([
            "inferscope",
            "--sample-only",
            "--pid",
            "4242",
            "--duration-secs",
            "30",
            "--metrics-endpoint",
            "http://localhost:8000/metrics",
            "--model",
            "Qwen/Qwen2.5-7B-Instruct",
            "--engine",
            "vllm",
        ])
        .expect("sample-only with metrics-endpoint + model should parse");
        assert!(args.sample_only);
        assert_eq!(args.pid, Some(4242));
        assert_eq!(args.duration_secs, Some(30));
        assert_eq!(
            args.metrics_endpoint.as_deref(),
            Some("http://localhost:8000/metrics")
        );
        assert_eq!(args.model.as_deref(), Some("Qwen/Qwen2.5-7B-Instruct"));
    }

    #[test]
    fn sample_only_requires_duration_secs() {
        // --sample-only without --duration-secs must fail (required_if_eq).
        let result = Args::try_parse_from(["inferscope", "--sample-only", "--pid", "4242"]);
        assert!(result.is_err());
    }
    #[test]
    fn engine_is_required_with_a_metrics_endpoint() {
        // ADR-014 D6: the two vocabularies name different series, so a
        // scrape without a declared engine is a caller error rather
        // than a silent default to vLLM.
        let err = Args::try_parse_from([
            "inferscope",
            "--sample-only",
            "--pid",
            "1",
            "--duration-secs",
            "5",
            "--metrics-endpoint",
            "http://localhost:8000/metrics",
            "--model",
            "m",
        ])
        .expect_err("--metrics-endpoint without --engine should be rejected");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }
    /// Parses a sample-only invocation that scrapes, with the given
    /// engine flags appended. Keeps the four resolution tests to their
    /// one differing input.
    fn engine_resolution(extra: &[&str]) -> Result<Option<Engine>, String> {
        let mut argv = vec![
            "inferscope",
            "--sample-only",
            "--pid",
            "1",
            "--duration-secs",
            "5",
            "--metrics-endpoint",
            "http://localhost:8000/metrics",
            "--model",
            "m",
        ];
        argv.extend_from_slice(extra);
        Args::try_parse_from(argv)
            .expect("clap should accept these arguments")
            .engine()
    }

    #[test]
    fn vllm_resolves_without_a_page_size() {
        assert_eq!(
            engine_resolution(&["--engine", "vllm"]),
            Ok(Some(Engine::Vllm))
        );
    }

    #[test]
    fn sglang_carries_the_declared_page_size() {
        // The value is not derivable from a scrape and selects the
        // accounting class, so it travels into the type (ADR-014 D6).
        assert_eq!(
            engine_resolution(&["--engine", "sglang", "--page-size", "16"]),
            Ok(Some(Engine::Sglang { page_size: 16 }))
        );
    }

    #[test]
    fn sglang_without_a_page_size_is_an_error() {
        let err = engine_resolution(&["--engine", "sglang"])
            .expect_err("sglang without --page-size should not resolve");
        assert!(err.contains("--page-size"), "{err}");
    }

    #[test]
    fn a_page_size_supplied_with_vllm_is_an_error() {
        // Absorbing it silently would be an input that does not apply,
        // accepted anyway — the failure mode D6 exists to prevent.
        let err = engine_resolution(&["--engine", "vllm", "--page-size", "16"])
            .expect_err("--page-size with vLLM should not resolve");
        assert!(err.contains("sglang"), "{err}");
    }
}

/// Modes that are not a profiling run.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Derive cost from an archived report at a declared rate.
    ///
    /// Reads measurements the report already carries and multiplies
    /// them by a rate supplied here. Nothing is measured, nothing is
    /// written back: cost stays outside the report by construction
    /// (ADR-015 D1), and the same report can be priced again later at
    /// a different rate without repeating the run.
    Cost {
        /// Path to a JSON report produced by a previous run.
        #[arg(long)]
        report: std::path::PathBuf,
        /// Declared price of the whole node, per hour. Occupancy
        /// basis: energy is already inside this price.
        #[arg(long, required_unless_present = "usd_per_kwh")]
        usd_per_hour: Option<f64>,
        /// Declared electricity price, per kilowatt-hour. Energy
        /// basis: for owned hardware, metered separately.
        ///
        /// Mutually exclusive with the hourly rate. One basis per
        /// derivation (ADR-015 D2): the two answer different
        /// questions and summing them would double-count on a rented
        /// node. To obtain both, invoke twice.
        #[arg(long, conflicts_with = "usd_per_hour")]
        usd_per_kwh: Option<f64>,
    },
}

/// Every run-level flag, by clap id.
///
/// Listed explicitly rather than derived: a flag added later should
/// force a decision about whether it belongs here, and a silent
/// omission is exactly the failure this guard exists to prevent.
const RUN_FLAG_IDS: &[&str] = &[
    "endpoint",
    "model",
    "prompt",
    "max_tokens",
    "pid",
    "sample_period_ms",
    "metrics_endpoint",
    "engine",
    "page_size",
    "metrics_period_ms",
    "steps_file",
    "include_descendants",
    "sample_only",
    "duration_secs",
    "json",
];

/// Run-level flags that exist only under a Cargo feature.
///
/// Interrogating an id clap does not know panics in debug builds, so
/// these are kept apart rather than listed unconditionally.
const FEATURE_RUN_FLAG_IDS: &[&str] = &[
    #[cfg(feature = "gpu-nvidia")]
    "gpu",
    #[cfg(feature = "otel-export")]
    "otel_endpoint",
];

/// Parses arguments, rejecting an invocation that asks for both a run
/// and a subcommand.
///
/// `subcommand_negates_reqs` makes the run flags optional, which also
/// makes them silently accepted alongside a subcommand. A caller who
/// appends `cost ...` to a variable already holding run flags would
/// get a cost derivation and no run, with nothing said about the
/// flags that were dropped. Provenance from `value_source`
/// distinguishes a flag that was written from one left at its
/// default, which a bool at `false` cannot.
pub fn parse_checked() -> Result<Args, String> {
    parse_checked_from(std::env::args_os())
}

/// The checked parse, over an explicit argument vector.
///
/// Split out from [`parse_checked`] so the guard is exercised in CI
/// rather than only by hand at a terminal.
pub fn parse_checked_from<I, T>(argv: I) -> Result<Args, String>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::{parser::ValueSource, CommandFactory, FromArgMatches};

    let matches = Args::command()
        .try_get_matches_from(argv)
        .map_err(|e| e.to_string())?;
    let args = Args::from_arg_matches(&matches).map_err(|e| e.to_string())?;

    if args.command.is_some() {
        // `EnvVariable` is deliberately not a conflict: an exported
        // OTLP endpoint is ambient configuration, not a run asked for
        // on this command line.
        let given: Vec<&str> = RUN_FLAG_IDS
            .iter()
            .chain(FEATURE_RUN_FLAG_IDS.iter())
            .copied()
            .filter(|id| matches.value_source(id) == Some(ValueSource::CommandLine))
            .collect();
        if !given.is_empty() {
            let flags: Vec<String> = given
                .iter()
                .map(|id| format!("--{}", id.replace('_', "-")))
                .collect();
            return Err(format!(
                "a subcommand does not take run flags, but {} {} given. \
                 The subcommand would have run and {} would have been \
                 ignored; drop one of the two.",
                flags.join(", "),
                if flags.len() == 1 { "was" } else { "were" },
                if flags.len() == 1 {
                    "the flag"
                } else {
                    "the flags"
                }
            ));
        }
    }
    Ok(args)
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn a_subcommand_next_to_run_flags_is_rejected() {
        let err = parse_checked_from([
            "inferscope",
            "--sample-only",
            "--duration-secs",
            "30",
            "--pid",
            "1",
            "cost",
            "--report",
            "r.json",
            "--usd-per-hour",
            "1.5",
        ])
        .unwrap_err();
        // Every offending flag is named: a caller who dropped one
        // would otherwise rediscover the next on the following run.
        assert!(err.contains("--sample-only"), "{err}");
        assert!(err.contains("--duration-secs"), "{err}");
        assert!(err.contains("--pid"), "{err}");
        assert!(err.contains("were given"), "{err}");
    }

    #[test]
    fn one_offending_flag_reads_as_singular() {
        let err = parse_checked_from([
            "inferscope",
            "--max-tokens",
            "64",
            "cost",
            "--report",
            "r.json",
            "--usd-per-hour",
            "1.5",
        ])
        .unwrap_err();
        assert!(err.contains("--max-tokens was given"), "{err}");
    }

    #[test]
    fn a_clean_subcommand_parses() {
        let args = parse_checked_from([
            "inferscope",
            "cost",
            "--report",
            "r.json",
            "--usd-per-hour",
            "1.5",
        ])
        .unwrap();
        assert!(args.command.is_some());
    }

    #[test]
    fn a_clean_run_still_parses() {
        let args = parse_checked_from([
            "inferscope",
            "--endpoint",
            "http://localhost:8000",
            "--model",
            "m",
            "--prompt",
            "p",
        ])
        .unwrap();
        assert!(args.command.is_none());
        assert_eq!(args.model.as_deref(), Some("m"));
    }

    #[test]
    fn run_flag_ids_covers_every_top_level_flag() {
        // A flag added to `Args` without being listed would be
        // silently accepted next to a subcommand, which is the exact
        // failure the guard exists to prevent.
        let cmd = Args::command();
        let actual: Vec<String> = cmd
            .get_arguments()
            .map(|a| a.get_id().to_string())
            .filter(|id| id != "help" && id != "version")
            .collect();
        let known = |id: &str| RUN_FLAG_IDS.contains(&id) || FEATURE_RUN_FLAG_IDS.contains(&id);
        let mut missing: Vec<&String> = actual.iter().filter(|id| !known(id)).collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "not listed in RUN_FLAG_IDS: {missing:?}"
        );

        let stale: Vec<&&str> = RUN_FLAG_IDS
            .iter()
            .chain(FEATURE_RUN_FLAG_IDS.iter())
            .filter(|id| !actual.iter().any(|a| a == *id))
            .collect();
        assert!(stale.is_empty(), "listed but no longer a flag: {stale:?}");
    }
}
