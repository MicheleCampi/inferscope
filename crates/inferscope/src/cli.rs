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
    #[arg(long)]
    pub endpoint: String,

    /// Model identifier as the engine expects it.
    #[arg(long)]
    pub model: String,

    /// Prompt sent as the single user message.
    #[arg(long)]
    pub prompt: String,

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
}

impl Args {
    /// Returns the resource sampling period as a `Duration`.
    pub fn sample_period(&self) -> Duration {
        Duration::from_millis(self.sample_period_ms)
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
        assert_eq!(args.endpoint, "http://localhost:8080");
        assert_eq!(args.model, "llama3");
        assert_eq!(args.prompt, "hello");
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
}
