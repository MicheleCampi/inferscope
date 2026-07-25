# Changelog

All notable changes to inferscope are recorded here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_Nothing yet._

## [0.4.0] — 2026-07-25

The OpenTelemetry export and sample-only mode that sat unreleased since
May ship here, together with the energy, KV-cache, per-phase and
per-trajectory work below. v0.3.0 tagged the per-device GPU metrics and
nothing more: none of the energy or attribution features the README
describes were in that tag, which is why this release exists.

### Added

- **Energy and efficiency metrics (ADR-010).** Total energy from the NVML
  hardware energy counter (`nvmlDeviceGetTotalEnergyConsumption`), with a
  trapezoidal integral of sampled power as an explicitly second-best
  fallback, flagged as such via `energy_source`. Derived
  `energy_per_token_mj`, `tokens_per_joule`, `tokens_per_watt`. Validated
  end-to-end on an A10 against llama.cpp — evidence in
  `validation-results/adr-010-a10-energy-counter.json`.
- **KV-cache hit rate from a Prometheus vLLM-schema endpoint (ADR-011),**
  in a new `is-metrics` crate — a third category of metric source next to
  host-side `/proc` sampling and the load-driving probe: read-only scraping
  of engine-internal counters across a network boundary. Hand-written
  parser with prefix guards over `vllm:prefix_cache_hits` /
  `vllm:prefix_cache_queries`, counter-reset handling, fixed scrape
  timeout. Fixture from `llm-d-inference-sim` v0.8.2 committed.
- **Per-phase energy attribution, prefill vs decode (ADR-012).** Two
  apportionments of the same energy — time-share and token-share — with
  their divergence exposed as the first-class signal rather than a single
  number restating its own premise.
- **Per-trajectory, per-step attribution for agentic workloads (ADR-013).**
  `--steps-file` ingests driver-emitted step boundaries (JSONL: `step_id`,
  `kind`, wall-clock start/end); `is-report` joins them offline against the
  sampled timelines. Per-step energy from segment-exact integration, per-step
  counter deltas, and an unattributed remainder that reconciles exactly to
  the whole-run figure. inferscope stays a passive observer: the driver and
  the profiler never communicate during the run.
- **Wall-clock anchor** (`reference_instant_unix_ns`) on `Report` and
  `ResourceReport`, without which no offline join is possible.
- **`validation-results/`** — captured hardware evidence with provenance
  (driver, versions, topology, server logs), indexed by a README that states
  what each run does *and does not* establish.

### Changed

- **BREAKING (JSON schema): the four counter deltas on `StepMetrics` are
  `Option<u64>`,** not `u64`. An absent timeline used to serialise as `0`,
  indistinguishable from a measured zero; it now reads `null`.
- **BREAKING (figures): per-step counter deltas take the baseline from the
  last sample at or before the window start,** not the first sample inside
  it. The old convention silently dropped any counter movement between the
  window start and its first interior sample — a systematic zero for
  counters that jump at step start (prompt tokens on prefill), and
  under-reporting for progressive counters. Found by auditing the A10
  evidence against the code that produced it; per-step figures in reports
  generated before this release are affected.
- `reqwest` moved from `native-tls` to `rustls-tls`.

### Documentation

- ADR-010 through ADR-013 added and indexed.
- ADR-013 amended: the delta-baseline semantics, the over-attribution cost
  of the correction, and absence-is-not-zero in the schema.
- ADR-012 and ADR-013 validation headers restated against what the
  hardware runs actually establish, including what remains unvalidated.

### Previously under Unreleased


### Added

- **Sample-only mode (`--sample-only`).** Attaches to an already-running process via `--pid` and samples its CPU/RSS (and per-device GPU usage when built with `gpu-nvidia` and `--gpu`) for a fixed `--duration-secs`, WITHOUT issuing any inference request. Intended for profiling a server while an external load generator (e.g. AIPerf) drives the traffic — the case the active probe cannot serve without perturbing the measurement. Emits a dedicated resource-only JSON report (`ResourceReport`), not a degraded `Report`. In this mode `--endpoint`, `--model`, and `--prompt` are not required. Design recorded in ADR-009.
- **Public type `is_report::ResourceReport` and `is_report::render_resource_json`.** A standalone resource-sampling report (pid, sampling parameters, derived `ResourceMetrics`, optional per-device `GpuMetrics`) with no `request_timing`, reusing the existing derived-metric types unchanged.
- **OpenTelemetry export via OTLP/HTTP.** Built with `--features otel-export`, inferscope can emit a derived report as a single OpenTelemetry trace to any OTLP/HTTP receiver (Jaeger, Tempo, Honeycomb, Datadog APM, OTel Collector). One root span `inferscope.run` carries the derived aggregates as attributes; each token arrival is attached as a span event named `token.arrival` with `token.index` and `token.elapsed_ns` attributes, timestamped at `run_start + elapsed_ns` so trace UIs render the per-token cadence on the timeline. Design recorded in ADR-008.
- **`--otel-endpoint` CLI flag** on the `inferscope` binary, available only when built with the `otel-export` feature. Accepts the base URL of the OTLP receiver (e.g. `http://localhost:4318`); the library appends `/v1/traces` per the OTLP/HTTP spec. The standard `OTEL_EXPORTER_OTLP_ENDPOINT` env var is honoured if the flag is not supplied.
- **Public function `is_report::export_to_otel(&Report, &str)`**, gated on the `otel-export` feature of the `is-report` crate. Returns `Result<(), OtelExportError>`; the CLI logs export failures to stderr without changing the exit code, so observability remains secondary to the profiling result.

### Documentation

- **ADR-009: Sample-Only Mode and the ResourceReport Type** added under `docs/adr/`, documenting why a separate output type is preferred over making `Report.request_timing` optional (each type models one concept; no regression to the probe path), and how the sampling primitives are reused with a timer-driven cancellation instead of probe completion.
- **ADR-008: OpenTelemetry Export of Inferscope Reports** added under `docs/adr/`, with Context (the operational gap that ADR-004's stdout-only contract leaves), Decision (one root span, token arrivals as events, OTLP/HTTP over hyper, opt-in feature flag), Consequences (positive/negative/neutral), and six Alternatives Considered (OTLP/gRPC and reqwest-client rejected for MSRV reasons, multiple child spans per token rejected as the wrong OTel-semantic shape, OTel Metrics signal deferred, OTLP/JSON rejected for ecosystem maturity, hand-rolled OTLP rejected for maintenance cost, separate is-otel crate rejected as over-fragmentation).
- **README.md** gains a Quick example block showing the Jaeger all-in-one Docker setup, the inferscope invocation with `--otel-endpoint`, and what the resulting trace looks like in the Jaeger UI. The Building section gains a new Optional Cargo features list documenting both `gpu-nvidia` and `otel-export` with a combine-features example.
- **RUNBOOK.md** gains Scenario 8 — "OpenTelemetry export failed", following the same Detection / Diagnosis / Fix / Root cause / Prevention structure as the other scenarios. Five diagnosis steps and five fixes covering the predictable transport-layer failure modes.

### Internal

- **`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp` 0.32** added as workspace dependencies, pinned to the same minor version to keep the trio coordinated. The OTLP exporter uses the `http-proto` + `hyper-client` Cargo features rather than `reqwest-client`, because the reqwest path transitively pulls reqwest 0.13 which requires Rust 1.85; hyper keeps the workspace MSRV at 1.83.
- **`clap` workspace dependency** gains the `env` feature, enabling the `env = "OTEL_EXPORTER_OTLP_ENDPOINT"` attribute on the new CLI flag and making the standard env var support available to any future arg.
- **Unit test for the error path** of `export_to_otel`: a syntactically invalid endpoint URL must produce an `Err`. The test wraps the call in a single-threaded tokio runtime since the function builds a hyper HTTP client internally and hyper requires an active tokio reactor. A second test against an unreachable port was prototyped but removed: opentelemetry-otlp 0.32's shutdown path waits past the build-in timeout when the collector is unreachable, hanging the test process. A proper end-to-end test using wiremock is tracked as a follow-up.


## [0.3.0] — 2026-05-25

### Added

- **Per-device GPU metrics in the JSON output.** The `gpu` section now
  includes a `per_device: Vec<GpuDeviceMetrics>` field with one entry
  per distinct `device_index` in the timeline, sorted ascending. Each
  entry carries the same set of aggregates the top-level cluster block
  computes (`sample_count`, VRAM min/max/mean, `memory_total_bytes`,
  utilisation min/max/mean, peak temperature, power peak/mean), but
  restricted to one device's samples. Cluster-wide aggregates remain
  unchanged for backward compatibility. The new field is additive;
  consumers reading only the top-level `gpu.*` continue to work.
  See [ADR-007](docs/adr/007-per-device-gpu-metrics.md).
- **`GpuDeviceMetrics` struct** in `is-report::metrics`. Twelve fields:
  `device_index`, `sample_count`, plus the ten metric fields that
  match `GpuMetrics`. Implements `Debug, Clone, PartialEq, Eq,
  Serialize, Deserialize`. Not `Copy` (intentional: the type appears
  in `Vec<GpuDeviceMetrics>` which is not `Copy`-compatible).
- **Per-device GPU block in the text report.** When `device_count > 1`,
  `render_text` emits a "Per-device GPU usage:" block after the
  existing cluster-wide block, with one compact line per device:
  `GPU N:  VRAM <peak> | SM mean <%> | power mean <W> | temp peak <C>`.
  Single-GPU runs produce identical output to v0.2.x (no new block).
- **CI green build restored.** Two issues introduced silently with the
  v0.2.0 release (20 May) had left the `fmt` and `clippy` CI jobs red
  on `main` for nine days, even though `cargo test` continued to pass.
  Root causes: (1) `crates/is-sysmon/src/sampler.rs:209` used
  `assert_eq!(..., true)` which triggers `clippy::bool_assert_comparison`
  under the workspace's `-D warnings` policy, replaced with `assert!(...)`;
  (2) seven files across the workspace had drifted from `rustfmt`
  conformance, brought back in line with `cargo fmt --all`. Pure
  formatting and one test-assertion idiom — no behavioural change.
  A local `pre-push` git hook was added to the development environment
  to run `cargo fmt --all --check` and `RUSTFLAGS="-D warnings" cargo
  clippy --workspace --all-targets` before every push, blocking pushes
  that would re-introduce the same class of regression. The `README.md`
  claim of "CI gated on `-D warnings`" is once again truthful.
- **`Dockerfile`** at the repository root. Multi-stage build:
  `rust:1.83-slim` compiles the `gpu-nvidia`-featured release binary,
  `nvidia/cuda:13.0.2-runtime-ubuntu22.04` hosts only the binary +
  `ca-certificates` for HTTPS calls to the inference endpoint. Runs as
  non-root user `inferscope` (UID 1000). Final image: ~1.65 GB
  compressed, verified buildable on Ubuntu 24.04 with Docker 29.5.2.
  `ENTRYPOINT` is the binary, `CMD ["--help"]` so `docker run
  inferscope` prints usage. OCI image labels populated for
  `org.opencontainers.image.{title,description,source,licenses,authors}`.
- **GitHub Action `docker-publish.yml`** that builds the Dockerfile
  and pushes the image to GHCR on git tag push matching `v*.*.*`, or
  on manual `workflow_dispatch`. Uses `docker/setup-buildx-action@v3`,
  `docker/login-action@v3`, `docker/metadata-action@v5`, and
  `docker/build-push-action@v6` with GitHub Actions cache
  (`type=gha, mode=max`) for incremental rebuilds. Auto-generated tags
  on push of `v0.3.0`: `0.3.0`, `0.3`, `0`, `latest`. The image is
  publicly pullable from `ghcr.io/michelecampi/inferscope`.
- **`deploy/` directory** with example deployment manifests:
  `docker-compose.yml` for local runs with NVIDIA Container Toolkit,
  `inferscope-job.yaml` as a Kubernetes Job example (NVIDIA Device
  Plugin resource request, `backoffLimit: 0`, commented `nodeSelector`
  for GPU-pool targeting), and `README.md` documenting the design
  trade-offs (Job vs Deployment, no-retry policy, image pinning) and
  what the directory does not cover (CronJob, Sidecar, Helm, image
  signing). Explicitly framed as example material, not production
  configuration.
- **`benchmarks/` directory** with three files of verified
  cross-hardware data: `cross-hardware-comparison.md` (L4 vs H100 vs
  4×A40 on Qwen 2.5 0.5B/7B/32B Q4_K_M), `multi-device-validation.md`
  (4×A40 TP=2 single-socket vs TP=4 cross-socket deep dive),
  `vllm-vs-llama-cpp.md` (vLLM 0.21 vs llama.cpp b9165 head-to-head
  on H100 with cold/warm-outlier/warm-steady three-run methodology).
  All numbers pulled directly from inferscope JSON output and the
  per-run summary report; the discrepancy between aggregate and
  per-device readings on multi-GPU runs is documented as the
  motivation for ADR-007.
- **`SECURITY.md`** with explicit threat model, controls, and known
  limitations (single-maintainer SPOF, unsigned image, no SAST/fuzz
  coverage). Cross-references `RUNBOOK.md` for operational failure
  modes and ADR-005 for the GPU sampling threat surface.
- **`RUNBOOK.md`** at the repository root with seven failure scenarios
  drawn from real RunPod validation runs, each structured Detection →
  Diagnosis → Fix → Root Cause → Prevention. Scenarios cover the
  wrapper-PID pitfall, GPU sampling unavailable, connection refused
  from the engine, HTTP 4xx/5xx from the endpoint, sparse GPU samples,
  build failures with the `gpu-nvidia` feature, and Docker GPU
  passthrough failures.
- **Architecture diagram in README.** Mermaid `flowchart LR` showing
  the runtime data flow: operator → CLI → is-probe (untrusted endpoint
  in red) and is-sysmon (trusted `/proc` and NVML in green) → is-report
  → stdout + JSON. Replaces the text-only Architecture description.
- **Documentation section in README** with a table linking the eight
  major reference assets: SECURITY, RUNBOOK, CHANGELOG, the ADR
  directory, the RunPod GPU validation runbook, the Dockerfile,
  the `deploy/` directory, and the `benchmarks/` directory.

### Changed

- **`GpuMetrics` no longer derives `Copy`.** The new `per_device:
  Vec<GpuDeviceMetrics>` field is not `Copy`-compatible. Consumers
  that assigned `GpuMetrics` by copy semantics (`let m2 = m1;`) now
  move it; the standard Rust pattern. No call site in the workspace
  was affected. The struct still derives `Debug, Clone, PartialEq,
  Eq, Serialize, Deserialize`.

## [0.2.1] — 2026-05-21

### Added

- **`--include-descendants` CLI flag** on the `inferscope`
  binary. When supplied together with `--pid`, the `/proc`
  sampler aggregates the monitored PID with the resource usage
  of every direct child the kernel reports under
  `/proc/<pid>/task/<pid>/children`. Addresses the wrapper-PID
  class of failure where a forked-worker process model (typical
  of `llama-server`, `uvicorn`, `gunicorn`, `vllm`) leaves the
  parent process reporting near-zero RSS / CPU / threads. The
  pre-existing runtime warning from v0.2.0 (`monitored PID
  looks idle…`) is what tells the user to try the new flag.
  See [ADR-006](docs/adr/006-process-tree-aggregation.md).
- **`SysmonConfig::include_descendants`** field (default
  `false`) and **`SysmonConfig::with_descendants()`** builder
  method, exposing the aggregation opt-in to library consumers.
- **`is_sysmon::parse::parse_children`** — pure parser for the
  kernel's `/proc/<pid>/task/<tid>/children` file format. Five
  unit tests cover the empty case, single PID, multiple PIDs,
  trailing whitespace, and the non-numeric token error path.
- **`is_sysmon::sampler::sample_once_aggregated`** — sampling
  primitive that reads `/proc` for the parent PID and every
  direct child, summing the four numeric `ResourceSample`
  fields. Uses saturating arithmetic on every field for cheap
  overflow safety. Failure-tiered behaviour: parent unreadable
  propagates, children file unreadable falls back to parent-only,
  per-child unreadable silently skipped (race tolerance per
  ADR-006).
- **Integration test** that spawns a real bash + sleep parent-
  child pair, samples the bash PID with and without
  aggregation, and verifies the aggregated thread count is
  strictly higher than the parent-only count. End-to-end
  exercise of the path that crosses `parse_children` →
  `sample_once_aggregated` → `ResourceSample` summation.

### Changed

- **`sample_during`** now dispatches to `sample_once_aggregated`
  when its `SysmonConfig` has `include_descendants = true`,
  and to `sample_once` otherwise. Five-line change inside the
  existing tick branch; surrounding `select!` / cancellation
  / best-effort logic untouched.

### Compatibility

- **Fully backward compatible with v0.2.0.** The new
  `SysmonConfig` field has a default that preserves v0.1.0 /
  v0.2.0 semantics exactly. Every existing constructor
  (`SysmonConfig::new`, `SysmonConfig::with_period`) produces
  a config with aggregation disabled. No call site in the
  workspace constructs `SysmonConfig` via literal struct
  syntax, so the additive field cannot break downstream code.
  Users who do not pass `--include-descendants` see no change
  in CLI behaviour, report format, or JSON schema.

### Known limitations

- **Direct children only.** Aggregation walks one level: the
  PIDs in `/proc/<pid>/task/<pid>/children`. Grandchildren are
  not included. This covers the inference engines inferscope
  targets (one-level fork models); a future revision may add
  recursive walking if a production deployment surfaces the
  need.
- **Per-PID detail not exposed.** The aggregated sample sums
  the group; the report cannot say "the parent held 100 MiB,
  the worker held 1 GiB" — only "the group held 1.1 GiB". A
  future revision may add per-PID breakdown to the JSON output
  behind a verbosity flag.

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

[Unreleased]: https://github.com/MicheleCampi/inferscope/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/MicheleCampi/inferscope/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/MicheleCampi/inferscope/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/MicheleCampi/inferscope/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/MicheleCampi/inferscope/releases/tag/v0.1.0
