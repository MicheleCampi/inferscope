//! Command-line argument parsing.
//!
//! The CLI surface is intentionally small. The flags map directly
//! to fields of `ProbeConfig` and `SysmonConfig`, with one
//! orchestrator-level flag (`--pid`) and one output flag
//! (`--json`). When the `gpu-nvidia` Cargo feature is enabled,
//! an additional `--gpu` flag becomes available.

use std::time::Duration;

use clap::Parser;

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
    #[arg(long)]
    pub metrics_endpoint: Option<String>,
    /// Scrape period for `--metrics-endpoint`, in milliseconds.
    ///
    /// Defaults to 1000 ms — deliberately slower than the 50 ms
    /// resource-sampling cadence, since a `/metrics` scrape is an HTTP
    /// round-trip reading per-request application counters (ADR-011).
    #[arg(long, default_value_t = 1000)]
    pub metrics_period_ms: u64,

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
}

impl Args {
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
}
