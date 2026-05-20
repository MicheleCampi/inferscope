# Changelog

All notable changes to inferscope are recorded here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] — 2026-05-20

### Added

- **NVIDIA GPU resource sampling** via NVML. When built with
  `--features gpu-nvidia` and invoked with `--gpu`, inferscope
  now samples every visible NVIDIA GPU in parallel with the
  probe and the `/proc` sampler, recording VRAM in use, SM
  utilisation, temperature, and power draw. Samples carry
  `elapsed_ns` from the same reference instant the probe and
  the `/proc` sampler use, so GPU samples correlate with token
  arrivals and CPU samples by direct numeric comparison. See
  [ADR-005](docs/adr/005-gpu-resource-sampling.md).
- **`GpuSample` and `GpuTimeline`** in `is-core`, parallel in
  shape to the existing `ResourceSample` and `ResourceTimeline`.
  Integer fields throughout — VRAM in bytes, utilisation as
  `0..=100`, power in milliwatts — preserving the lossless-
  signal principle from ADR-002 and ADR-003.
- **`GpuSampler`** in `is-sysmon`, behind the `gpu-nvidia`
  feature flag. Construction initialises NVML once and caches
  per-device indices; per-tick sampling re-fetches handles
  cheaply (microseconds per device). Fails fast and gracefully
  on hosts without an NVIDIA driver, surfacing
  `GpuError::NvmlUnavailable` rather than aborting the run.
  No `unsafe` anywhere in the crate.
- **`GpuMetrics`** derived metrics in `is-report`: VRAM
  aggregations (min/max/mean/total), SM utilisation aggregations
  (min/max/mean), peak temperature, and power draw (peak/mean).
  All-integer field types, so the struct derives `Eq`.
- **`--gpu` CLI flag** on the `inferscope` binary, compiled in
  only when the `gpu-nvidia` feature is enabled. Defaults to
  false; when set, the orchestrator spawns the GPU sampler
  alongside the probe and the `/proc` sampler with shared
  cancellation semantics.
- **Plain-text GPU section** in the report output ("GPU
  resource usage") with per-device count, VRAM peak/mean/min
  and total, SM utilisation peak/mean/min, peak temperature,
  and power peak/mean. JSON output carries both the raw
  `gpu_timeline` and the derived `gpu` metrics in the same
  document per ADR-004.
- **Pre-flight warning for misconfigured `--pid`**. When every
  sample in the timeline shows RSS below 10 MiB AND exactly one
  thread AND zero CPU jiffies, inferscope now emits a stderr
  warning that the supplied PID likely points to a wrapper shell
  rather than the actual workload (a common pitfall with bash
  background launches followed by output redirection). The
  warning is informational and does not alter the report.
- **GPU validation runbook** in `docs/runbooks/runpod-gpu-validation.md`,
  a step-by-step procedure for validating the NVIDIA path on a
  real GPU host via RunPod. Documents the correct `pgrep -x`
  pattern for capturing the `llama-server` PID, the
  `$!`-after-redirection pitfall it avoids, cost estimates per
  validation run, and a troubleshooting section covering the
  most likely first-time failure modes.

### Changed

- The `Report` struct in `is-report` gains an
  `Option<GpuTimeline>` raw field and an `Option<GpuMetrics>`
  derived field. Both are `None` when the GPU path is inactive
  (no `--gpu` flag, NVML unavailable, or feature not compiled
  in); the text and JSON renderers omit the GPU section in
  that case.

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
- **Pre-flight `--pid` validation.** When `--pid` is supplied,
  the orchestrator verifies that `/proc/<pid>` exists before
  starting the probe. An invalid PID fails fast with a clear
  error and a non-zero exit code; no request is sent to the
  engine.
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
- **PID identity is not verified.** The CLI checks that the
  supplied `--pid` corresponds to a live process before starting
  a run, but does not verify that the process is in fact the
  intended inference engine. If the user passes the PID of an
  unrelated process, sysmon will faithfully sample that process.
  A user must still ensure the PID is the right one.

[Unreleased]: https://github.com/MicheleCampi/inferscope/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/MicheleCampi/inferscope/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/MicheleCampi/inferscope/releases/tag/v0.1.0
