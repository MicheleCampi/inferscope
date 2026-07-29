# inferscope — Operational and Troubleshooting Guide

This document describes how to diagnose and respond to the seven failure modes operators most commonly hit when running `inferscope`. Each scenario follows a fixed structure — Detection, Diagnosis, Fix, Root Cause, Prevention — so the operator can act quickly without re-deriving the procedure each time.

Unlike a production service runbook, `inferscope` is a CLI that runs to completion and exits. "Incidents" here are reproducible failure modes a user encounters during a profiling run, not 3 a.m. alerts. The structure is the same; the urgency is not.

The guide is intentionally specific to inferscope's architecture (5-crate Rust workspace, `gpu-nvidia` opt-in feature, `/proc` sampler, NVML wrapper, OpenAI-compatible HTTP probe). Generic CLI troubleshooting boilerplate has been omitted.

## Conventions

- **Detection** lists the signals that should trigger you to open this scenario. If none of these signals are present, you're probably looking at a different scenario.
- **Diagnosis** is the first 2 minutes: a fixed sequence of checks to confirm the scenario class and exclude alternatives.
- **Fix** is the action that resolves the immediate failure.
- **Root cause** is the underlying mechanism. Useful for postmortems and for deciding whether a fix is durable.
- **Prevention** is the durable change (CLI flag default, doc update, code fix) that reduces the chance of the same failure recurring for the next user.

Throughout, the working directory is assumed to be the engine host (`ssh`'d into a RunPod box, or local). The endpoint is the OpenAI-compatible API of the engine under test.

---

## Scenario 1 — "Monitored PID looks idle"

The classic wrapper-PID failure. `llama-server`, `vllm serve`, `uvicorn`, `gunicorn`, and similar production-style engines fork worker processes; the PID you see in `ps` is the parent supervisor, which itself does almost nothing. Sampling the parent shows ~0 RSS, ~0 CPU, ~1 thread for the entire run, even though the engine is clearly working.

### Detection

- The stderr warning `inferscope: warning: monitored PID looks idle across all N samples (RSS < 10 MiB, single-threaded, zero CPU)` appears at the end of the run.
- The JSON report shows `samples` with `rss_bytes < 10000000`, `thread_count: 1`, `cpu_user_jiffies: 0` across the entire timeline.
- Yet the model clearly responded (the probe shows non-zero `tokens_emitted`).

### Diagnosis (2 minutes)

1. Run `ps -ef --forest | grep <engine-name>` to see the process tree. If you see multiple PIDs under your `--pid` target, you have a wrapper.
2. Identify which child PID actually owns the CPU work — `top -p $(pgrep -d, -P <parent-pid>)` reveals it.
3. Confirm the engine: this affects every production-grade Python LLM server (`vllm`, `llama-server` when built with multiple workers, `text-generation-inference`) and most forking webservers in front of inference code.

### Fix

Re-run with `--include-descendants`:

```
inferscope --endpoint ... --model ... --prompt ... \
           --pid <parent-pid> --include-descendants --gpu
```

The `/proc` sampler will now aggregate the parent PID with every direct child reported in `/proc/<pid>/task/<pid>/children`. RSS, CPU, and thread counts will reflect the actual workload.

### Root cause

The `/proc/<pid>` accounting in Linux is per-PID, not per-process-tree. A forking supervisor that does no work itself reports zero work even when its descendants are saturated. `inferscope` v0.1.x and v0.2.0 sampled only the supplied PID, leaving wrapper-process workloads unobservable. v0.2.1 introduced `--include-descendants` and the `parse_children` parser specifically to handle this; see [ADR-006](docs/adr/006-process-tree-aggregation.md).

### Prevention

For users: when in doubt, always pass `--include-descendants`. It is a no-op when there are no children (no overhead), and it is correct when there are.

For the project: a future version may auto-detect the wrapper case and either prompt or default to descendant-aggregation. Tracked, not yet scheduled.

---

## Scenario 2 — "GPU sampling requested but unavailable"

The `--gpu` flag was passed but inferscope cannot reach NVML.

### Detection

- The stderr warning `inferscope: warning: GPU sampling requested but unavailable: <error>` appears near the start of the run.
- The JSON report contains no `gpu` section, or an empty one.
- The plain-text report shows only CPU/memory metrics.

### Diagnosis (2 minutes)

1. Check the feature: `inferscope --version` and confirm the binary was built with `--features gpu-nvidia`. A binary without the feature does not link NVML at all and will silently skip GPU sampling.
2. Check the driver: `nvidia-smi` — if this fails, NVML is not reachable from userland (driver missing, container without `--gpus all`, NVIDIA Container Toolkit not installed).
3. Check the library path: `ldconfig -p | grep nvidia-ml` should show `libnvidia-ml.so.1`. On some minimal containers this is absent.
4. Check permissions: `ls -la /dev/nvidia*`. The process must be able to read these device files. Inside containers, this requires `--gpus all` or equivalent device passthrough.

### Fix

- **Feature missing**: rebuild with `cargo build --release --features gpu-nvidia` (or pull the Docker image, which always includes the feature).
- **Driver missing on host**: install the NVIDIA driver. This is out of scope for `inferscope`.
- **Container without GPU access**: re-run with `docker run --gpus all ...` or the Kubernetes equivalent.
- **Library missing**: install `libnvidia-ml1` from your distribution's package manager.

### Root cause

NVML access requires three things to align: the binary must link NVML (feature flag), the host must have the NVIDIA driver, and the process must have device access to the GPUs. Any of the three failing produces the same symptom from `inferscope`'s perspective: the wrapper returns an error on init.

### Prevention

The published Docker image carries the `gpu-nvidia` feature by default. Users running it on a GPU host with `docker run --gpus all` should not hit this scenario. Users building from source on GPU-less workstations should expect this and treat it as expected behavior, not an error.

---

## Scenario 3 — "Connection refused" or endpoint unreachable

The probe cannot reach the configured `--endpoint`.

### Detection

- `inferscope` exits with an error containing `error sending request`, `connection refused`, or `failed to lookup address`.
- The run terminates before any sampling occurs.

### Diagnosis (2 minutes)

1. Verify the endpoint is up: `curl -i <endpoint>/v1/models`. A 200 with a JSON model list means the engine is reachable.
2. Verify the port: many users hit this because they used the default OpenAI port (`8000`) when the engine is on a different one (`8080` for llama-server default, `8000` for vLLM default, etc.).
3. Verify network reachability: from the host running `inferscope`, can you `curl` the endpoint? If `inferscope` and the engine are on different containers/hosts, network isolation may be the issue.

### Fix

- Wrong port: pass the correct `--endpoint http://host:port` URL.
- Engine not started or still loading: wait. Large models take 30–120 seconds to load on first start. Re-run `inferscope` after the engine logs `Application startup complete` or equivalent.
- Network isolation: run `inferscope` inside the same network namespace as the engine, or expose the engine on a reachable interface.

### Root cause

`inferscope` does not include startup-wait logic. It assumes the endpoint is already serving when invoked. This is intentional: profiling the startup transient is a separate concern from profiling steady-state inference.

### Prevention

When scripting profiling runs, wait for `/v1/models` to return 200 before invoking `inferscope`. A simple `until curl -sf <endpoint>/v1/models; do sleep 1; done` works.

---

## Scenario 4 — HTTP 4xx or 5xx from the endpoint

The probe connected but the engine rejected the request.

### Detection

- `inferscope` exits with an error mentioning a non-2xx status code.
- The error body usually contains the engine's structured response (e.g. vLLM returns a `{"error": {...}}` JSON).

### Diagnosis (2 minutes)

1. Read the error body. Most engines return a clear reason: unknown model name, prompt too long for the context window, malformed request, authentication required.
2. Confirm the model name matches what the engine actually serves. Run `curl <endpoint>/v1/models` and compare with what you passed via `--model`.
3. If the engine requires an API key (some hosted endpoints do), `inferscope` v0.2.x does not pass arbitrary headers. This is a known limitation.

### Fix

- **Wrong model name**: use the exact `id` from `/v1/models`. Be aware of subtle differences (`Qwen/Qwen2.5-7B-Instruct` vs `qwen2.5-7b-instruct`).
- **Prompt too long**: shorten the prompt, or raise the engine's context limit (often a startup flag like `--max-model-len`).
- **Auth required**: at present, use an open endpoint, or run the engine without auth. Header pass-through is on the roadmap.

### Root cause

The probe is a thin wrapper over the OpenAI Chat Completions API. Anything the engine rejects at that layer surfaces here. `inferscope` does not retry — a single failure terminates the run, by design (retries would confound timing measurements).

### Prevention

Smoke-test the endpoint with `curl` before invoking `inferscope`, especially when validating against a new engine or model.

---

## Scenario 5 — GPU samples present but sparse, or values look wrong

The run completed, the JSON has a `gpu` section, but the samples are too few, or the values are implausible (e.g. utilization stuck at 0%, power draw constant, memory not changing).

### Detection

- The JSON report's `gpu.samples` has fewer entries than expected for the run duration divided by `--sample-period-ms`.
- Utilization or memory values are flat across the entire timeline despite the engine clearly working.
- Power draw shows the GPU's idle baseline only.

### Diagnosis (2 minutes)

1. Check sample period: lower than 50 ms (the default) may not give NVML enough time to refresh metrics. NVML internal cadence is around 100 ms for utilization on most consumer GPUs and lower on datacenter GPUs (H100, A100, A40).
2. Check device index: by default, `inferscope` samples GPU 0. On multi-GPU hosts, the engine may be running on a different device. v0.2.x samples all visible GPUs and aggregates; per-device breakdown lands in v0.3 (ADR-007 pending).
3. Confirm with `nvidia-smi -l 1` in a separate terminal during a re-run. If `nvidia-smi` shows activity but `inferscope`'s JSON does not, that's a real bug — report it.

### Fix

- Sample period too aggressive: use the default 50 ms or raise it (`--sample-period-ms 100`).
- Wrong device: set `CUDA_VISIBLE_DEVICES` when starting the engine to ensure it uses the device you expect. NVML sees all devices; the engine respects `CUDA_VISIBLE_DEVICES`.
- For multi-GPU with `tensor_parallel_size > 1` in vLLM, the aggregate is correct in v0.2.x but does not show how load splits. Wait for v0.3, or examine raw samples in the JSON directly.

### Root cause

NVML reports utilization as a sampled percentage over its own internal window. Very fast probes do not see new data each call. The choice of 50 ms as default is documented in [ADR-003](docs/adr/003-sample-period.md) as a compromise between resolution and observability.

### Prevention

Stick with the default sample period unless you have a specific reason to change it. Inspect the raw JSON when in doubt — the aggregate report hides per-sample detail.

---

## Scenario 6 — Build fails with `gpu-nvidia` feature

`cargo build --release --features gpu-nvidia` fails.

### Detection

- The build emits errors mentioning `nvml-wrapper-sys`, `bindgen`, `libnvidia-ml`, or `wrapper.h`.
- The build succeeds without the feature (`cargo build --release`).

### Diagnosis (2 minutes)

1. Verify NVML headers are installed: `dpkg -l | grep nvidia-ml-dev` on Debian/Ubuntu, or check that `/usr/include/nvml.h` exists. Without these, `nvml-wrapper-sys`'s `bindgen` step cannot run.
2. Verify the Rust toolchain matches MSRV: `cargo --version` should report 1.83 or later (see `rust-toolchain.toml`).
3. Verify the NVIDIA driver version is recent enough: `nvidia-smi --query-gpu=driver_version --format=csv,noheader`. Driver mismatch with CUDA runtime in headers can fail at build.

### Fix

- **Headers missing**: `apt-get install libnvidia-ml-dev` (Ubuntu/Debian) or distribution equivalent. The Dockerfile sidesteps this by building inside `rust:1.83-slim` with NVML resolved through the linker against the published `nvml-wrapper-sys` crate.
- **Wrong toolchain**: install Rust 1.83 via `rustup`. The `rust-toolchain.toml` should auto-resolve this on `cargo` invocations.
- **Driver too old**: upgrade the NVIDIA driver to a version compatible with the CUDA runtime used by the build environment.

### Root cause

The `gpu-nvidia` feature pulls in `nvml-wrapper`, which depends on `nvml-wrapper-sys`. The `-sys` crate uses `bindgen` to generate Rust bindings from `nvml.h` at build time. This requires both the header file and a working `libclang` for `bindgen` to parse it.

### Prevention

Use the Dockerfile for reproducible builds. If you must build on bare metal, install `libnvidia-ml-dev` and `clang` before invoking `cargo build --features gpu-nvidia`.

---

## Scenario 7 — Docker container cannot see GPU

The `inferscope` Docker image runs, but `--gpu` reports zero devices.

### Detection

- Container starts, binary runs.
- The stderr warning "GPU sampling requested but unavailable" appears.
- `docker run --rm --gpus all nvidia/cuda:13.0.2-runtime-ubuntu22.04 nvidia-smi` succeeds — the host has GPU access.
- But `docker run --rm --gpus all inferscope:<tag> --gpu ...` fails.

### Diagnosis (2 minutes)

1. Verify `--gpus all` was passed to `docker run`. Without it, Docker does not expose GPUs to the container, regardless of the image.
2. Verify NVIDIA Container Toolkit is installed on the host: `nvidia-ctk --version`. Without it, the `--gpus` flag is ignored or errors.
3. Run `nvidia-smi` inside the same container image as a sanity check: `docker run --rm --gpus all inferscope:<tag> --entrypoint nvidia-smi`. If this fails too, the issue is at the Docker/host layer, not in `inferscope`.

### Fix

- **Flag missing**: re-run with `--gpus all`.
- **Toolkit missing**: install NVIDIA Container Toolkit on the host. Documented at https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html.
- **Toolkit installed but Docker not restarted**: `systemctl restart docker` after toolkit install.
- **K8s context**: pod spec must include the `nvidia.com/gpu` resource request; the device plugin handles the rest.

### Root cause

GPU passthrough to containers is a host-and-runtime configuration concern, not an image concern. The `inferscope` image is identical whether or not the host can pass GPUs through; the failure is upstream of the binary.

### Prevention

The repository's `/deploy/` folder (when published) will include a working `docker-compose.yml` and Kubernetes Job manifest with the GPU resource request correctly set, so users can copy-paste a known-good configuration.

## Scenario 8 — OpenTelemetry export failed

`--otel-endpoint` was supplied but `inferscope` reports the export did not succeed.

### Detection

- The stderr warning `inferscope: OpenTelemetry export failed: <error>` appears immediately after the report is printed.
- The Jaeger / Tempo / Honeycomb UI shows no `inferscope.run` span for this run.
- The exit code is still `0` — export failure does not fail the run by design (per ADR-008).

### Diagnosis (2 minutes)

1. Check the feature: the binary must be built with `--features otel-export`. The published Docker image currently does not include this feature by default; a binary built with `cargo install inferscope` without the flag will not even expose `--otel-endpoint` in `--help`.
2. Check the endpoint shape: the value passed to `--otel-endpoint` is the **base URL** of the OTLP receiver (e.g. `http://localhost:4318`), **not** the full `/v1/traces` path. opentelemetry-otlp appends the path itself per the OTLP/HTTP spec.
3. Check the collector is reachable: `curl -v http://<host>:<port>/v1/traces` from the same machine. A `405 Method Not Allowed` is a success signal here — it confirms the endpoint exists and rejects GET. A connection refused, DNS failure, or TLS error tells you the receiver is not reachable.
4. Check the receiver supports OTLP/HTTP protobuf on the configured port. Jaeger all-in-one listens on `:4318` for OTLP/HTTP by default; the OTel Collector and Tempo defaults match. gRPC receivers on `:4317` will reject the protobuf-over-HTTP payload.
5. Inspect the error message: `SetupFailed(...)` means the builder rejected the URL syntactically; `ExportFailed { endpoint, message }` means the transport tried and got an error from the receiver.

### Fix

- **Feature missing**: rebuild from source with `cargo build --release --features otel-export`, or use a binary explicitly built with the feature. The Docker image will gain a `-otel` tag variant in a future release.
- **Wrong port**: switch the endpoint to the OTLP/HTTP port of the collector (Jaeger `:4318`, not `:14268` and not `:4317`).
- **Wrong path**: drop the `/v1/traces` suffix from the URL passed on the command line; the library adds it.
- **Collector down**: start the collector. For a quick local check, `docker run -d --rm --name jaeger -p 4318:4318 -p 16686:16686 jaegertracing/all-in-one:latest` is enough.
- **TLS to a collector behind HTTPS**: the current `otel-export` build uses hyper without TLS features. Either terminate TLS at a sidecar (envoy, an OTel Collector with `otlphttp` exporter as a hop), or wait for a future revision that enables `hyper-tls`.

### Root cause

The OTel export is a thin wrapper around opentelemetry-otlp 0.32. It can fail for the same reasons any HTTP client can fail: bad URL, no listener, wrong port, wrong path, no TLS support. The failure is logged and the run continues because, by design, observability is secondary to the profiling result — the JSON and text reports are still emitted on stdout regardless.

### Prevention

For a local development loop, run Jaeger all-in-one in a long-lived background container and point inferscope at it. For CI and shared environments, terminate TLS at an OTel Collector sidecar and have inferscope talk to it over plain HTTP on the loopback. Document the endpoint and port in the team's runbook so users do not guess.

Before trusting a build with this feature, confirm it compiles: `cargo build --release --features otel-export` is the command the README and this scenario both assume, and it was broken from 2026-07-18 to 2026-07-29 without any CI job noticing, because none built with `--all-features`. CI now carries an `all-features` job for exactly this; see CONTRIBUTING.md.

---

## Cross-cutting diagnostics

When in doubt, inspect the raw JSON output rather than the plain-text report. The plain-text report aggregates; the JSON preserves every sample, every timestamp, every NVML query result.

```
inferscope --endpoint ... --model ... --prompt ... \
           --pid <pid> --include-descendants --gpu \
           > run.txt 2> run.err
```

The plain-text report goes to stdout, warnings and errors to stderr. To capture the JSON instead, pass `--json` (writes JSON to stdout, plain-text suppressed).

For deeper investigation of the sampling layer, set `RUST_LOG=is_sysmon=debug` before invoking. This is verbose; redirect to a file.

For build issues, `cargo build --release --features gpu-nvidia --verbose` shows the full compiler invocation, including linker flags and NVML resolution.

## Changelog

- **2026-05-24**: Initial runbook published. Reflects v0.2.1 with `--include-descendants`, `gpu-nvidia` feature, Dockerfile multi-stage. Seven scenarios based on failure modes observed during L4 (RunPod), H100 (RunPod), 4×A40 multi-GPU (RunPod), and vLLM 0.21 (RunPod) validation runs over May 2026.
- **2026-05-26**: Added Scenario 8 for OpenTelemetry export failure, covering the `--otel-endpoint` flag introduced post-v0.3.0 (ADR-008). Scenario derived from local Jaeger validation and from the predictable transport-layer failure modes; no production OTel incidents have been observed yet.
