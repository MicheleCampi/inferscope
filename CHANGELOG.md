# Changelog

All notable changes to inferscope are recorded here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-05-16

The first public release of inferscope.

### Added

- **API-level profiling for OpenAI-compatible inference engines.**
  Drives an engine through its HTTP API and captures per-token
  timing. Works against llama.cpp's server, mistral.rs, vLLM,
  Ollama, TGI — anything that speaks the OpenAI streaming
  chat-completions protocol.
- **Process resource monitoring** via `/proc` sampling. When a
  `--pid` is supplied, samples the engine process's RSS, CPU
  time, and thread count at a configurable cadence (default
  50 ms) for the duration of the probe.
- **Derived metrics** computed from the raw signal: time-to-
  first-token, generation rate (tokens per second, excluding
  TTFT), inter-token latency distribution (mean, p50, p95, p99,
  max), RSS aggregations, mean CPU utilisation, thread range.
- **Two output formats.** Plain ASCII text targeted at terminal
  reading and copy-pasting into issues and pull requests; JSON
  carrying both the raw signals and the derived metrics so a
  consumer can recompute differently without re-running the
  probe.
- **CLI binary** that orchestrates probe and sysmon in parallel
  on a multi-thread tokio runtime, shares one reference instant
  between the two so samples and token arrivals are correlated
  by direct numeric comparison.
- **Five-crate Cargo workspace** with strict separation of
  concerns: `is-core` (pure data types), `is-probe` (network
  I/O), `is-sysmon` (filesystem I/O), `is-report` (pure
  presentation), `inferscope` (CLI).
- **Architecture Decision Records** documenting every
  significant design decision: profiling scope (ADR-001),
  token timing representation (ADR-002), sysmon scope and
  temporal correlation (ADR-003), report metrics and output
  format (ADR-004).
- **Apache-2.0 license**, MSRV pinned to Rust 1.83 via
  `rust-toolchain.toml`, CI on every push and pull request.

### Known limitations

- **CPU-side resource monitoring only.** The timing portion of
  inferscope is engine-agnostic and works against GPU engines
  today, but the resource portion currently reads `/proc` only.
  Against an engine running on a GPU the resource section
  describes the host process; GPU utilisation and VRAM are not
  reported. GPU resource monitoring (NVML / ROCm SMI) is
  planned for v0.2+. See [ADR-003](docs/adr/003-sysmon-scope-and-correlation.md).
- **Black-box profiling only.** v0.1.0 observes the engine from
  the outside, via its HTTP API and the operating system. It
  cannot attribute time or memory to specific phases inside the
  engine (KV cache management, attention computation, sampling
  loop). Engine-internal instrumentation is the v0.2+ direction.
  See [ADR-001](docs/adr/001-profiling-scope.md).
- **CPU peak not reported.** With 50 ms sampling and 100 Hz
  scheduler tick, per-sample CPU rate is dominated by
  quantisation noise. v0.1.0 reports the mean only. See
  [ADR-004](docs/adr/004-report-metrics-and-format.md).
- **PID validity is not enforced.** The CLI accepts a `--pid`
  argument and trusts it. If the supplied PID does not
  correspond to the engine process, sysmon will sample whatever
  it does correspond to. A user must ensure the PID is correct.

[Unreleased]: https://github.com/MicheleCampi/inferscope/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/MicheleCampi/inferscope/releases/tag/v0.1.0
