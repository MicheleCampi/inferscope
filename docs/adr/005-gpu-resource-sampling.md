# ADR-005: GPU Resource Sampling

- **Status**: Accepted
- **Date**: 2026-05-17
- **Deciders**: Michele Campi

## Context

ADR-003 deferred GPU monitoring to v0.2, with the architectural
door explicitly left open: "the sample type and the sampling
loop are not tied to `/proc`; v0.2 adds a parallel GPU sampler
whose output joins the same timeline." This ADR settles how that
door is walked through.

inferscope's primary subject — local LLM inference engines —
runs on GPUs in any production-grade deployment. For a CPU-only
profile against llama.cpp on a development VM, the
`/proc`-derived sample is the full resource story. Against a
vLLM server on an A100, it is half the picture. The half that
matters most, in fact: a forward pass is dominated by tensor
operations on the GPU, and the CPU-side process barely moves
while VRAM and SM utilisation swing.

Four decisions need to be settled before writing GPU sampling
code.

The first is what to read. Each GPU vendor exposes a different
control-plane API: NVIDIA's NVML, AMD's ROCm SMI, Intel's XPU
Manager, Apple's IOKit. NVIDIA covers nearly the entire LLM
inference market on rented GPU cloud (Lambda, RunPod, Vast,
CoreWeave) and most on-premises deployments. AMD is a credible
second tier and growing, especially with the MI300 family in
inference workloads. Intel and Apple are negligible for the
audience inferscope targets in v0.2. The two-vendor choice is
practical.

The second is how to access NVML from Rust. The library is a C
ABI shipped with the NVIDIA driver. Two paths exist: raw FFI
against `libnvidia-ml.so` (zero dependencies, full surface
exposed manually), or the `nvml-wrapper` crate (safe Rust API,
maintained, idiomatic). The second adds a dependency but pays it
back in correctness and readability.

The third is correlation. ADR-003 established the
shared-`Instant` invariant: every sample produced by every
sampler carries `elapsed_ns: u64` measured from one reference
instant captured by the orchestrator. The GPU sampler must
participate in the same scheme. There is no second clock to
reconcile.

The fourth, and the one that decides the shape of the user
experience, is fallback. inferscope must remain useful on
machines without a GPU — and specifically, must remain useful
on the CPU-only development VM. A v0.2 that requires NVML at
runtime to start a profile would regress v0.1.0's usability.
Conditional compilation alone is not sufficient; a binary
compiled with `--features gpu` must still run cleanly on a
laptop with no NVIDIA driver loaded.

## Decision

Four coordinated choices, plus an explicit alternative for AMD.

### What the GPU sampler reads

A `GpuSample` carries:

  - `elapsed_ns: u64` — nanoseconds since the shared reference
    instant. Same scheme as ADR-002 and ADR-003.
  - `device_index: u32` — GPU index on the host. A multi-GPU
    inference engine produces one sample per device per tick;
    consumers filter by index.
  - `memory_used_bytes: u64` — VRAM currently allocated, as
    reported by `nvmlDeviceGetMemoryInfo` (NVIDIA) or
    `rsmi_dev_memory_usage_get` (AMD). Stored in bytes for
    SI-comparable arithmetic, the same principle as
    `rss_bytes` in `/proc`.
  - `memory_total_bytes: u64` — VRAM capacity of the device.
    Constant across ticks for the same device but emitted on
    every sample so a consumer reading a single sample knows
    the device's total memory without reaching for separate
    metadata.
  - `utilization_percent: u8` — SM utilisation (NVIDIA) or
    GPU usage percent (AMD), 0–100. Reported as an integer
    because the underlying APIs expose it as such; preserving
    integer precision matches the jiffies argument from ADR-003.
  - `temperature_celsius: u32` — current chip temperature.
    Cheap to obtain on both vendors and useful for the rare
    case where thermal throttling is suspected of distorting
    inference latency.
  - `power_draw_milliwatts: u32` — current power draw in
    milliwatts. Stored as integer milliwatts (not watts as
    float) for the same reason as the percentage above.

VRAM and utilisation are the two fields that matter for
correlating with token timing. Temperature and power are
informational; their cost to sample is zero and a future article
or report might want them. They are emitted now rather than
added later to avoid a sample-type churn.

### NVML access via `nvml-wrapper`

The GPU sampler depends on `nvml-wrapper` rather than raw FFI.
The crate is actively maintained, covers the NVML API surface
inferscope needs, and reduces unsafe code in `is-sysmon` to
zero. The cost is one transitive dependency. The benefit is
that the GPU sampler's source reads like the `/proc` sampler's
source — declarative, error-handled idiomatically, free of
manual pointer arithmetic.

The `nvml-wrapper` initialisation is performed once when the
sampler is constructed, not per tick. The handle is reused for
the lifetime of the sampling loop. Per-device handles are
acquired once per sampling session, not per sample.

### Correlation: same reference instant, no new clock

The GPU sampler accepts the same `Instant` parameter the
existing `sample_during` accepts. Every `GpuSample` carries
`elapsed_ns` computed against that instant. The orchestrator
captures one `Instant` at the start of a probe run and hands
clones to the probe, the `/proc` sampler, and the GPU sampler.
Cross-sampler correlation is then a numeric comparison of
`u64` offsets — the same invariant ADR-003 established for the
two-sampler case, now scaled to three.

A consumer answers "what was VRAM utilisation when the third
token arrived?" with a binary search on GPU samples by
`elapsed_ns`, identical in shape to the CPU-side query. No
timestamp munging, no skew correction, no second clock.

### Graceful fallback when no GPU is present

The GPU sampler is feature-gated behind a Cargo feature
`gpu-nvidia`, off by default. With the feature off, no
NVML-related code is compiled and the binary has no link-time
dependency on `libnvidia-ml.so`.

With the feature on, runtime detection still applies. NVML
initialisation is fallible: it returns an error on a host where
the NVIDIA driver is not loaded. The sampler treats this as
"no GPU on this machine" rather than as a fatal error. In that
case the GPU sample stream is empty, the rest of inferscope
runs unaffected, and the report notes "no GPU samples collected
(NVML unavailable)" rather than crashing or claiming false zero
values.

This separation — compile-time feature off by default,
runtime-graceful when on — matches the way the rest of inferscope
handles optional capabilities. A consumer who builds the default
binary gets v0.1.0 behaviour exactly; a consumer who builds with
`--features gpu-nvidia` gets GPU sampling when a GPU is present
and the same v0.1.0 behaviour when one is not.

### AMD support via a parallel feature

`rocm-smi` access from Rust is a smaller ecosystem than NVML.
There is no widely-adopted safe wrapper crate; the typical path
is shelling out to the `rocm-smi` CLI and parsing its output,
or binding to the C library directly. Both are workable.

v0.2 adds AMD support behind a separate feature flag
`gpu-amd`. The implementation parses `rocm-smi --showuse
--showmeminfo vram --showtemp --showpower --json` output, which
is stable and machine-readable. The trade-off — running a
subprocess every tick instead of an in-process library call —
is acceptable because the AMD path is a secondary code path,
and because the cost of one fork+exec per tick is in the same
order as the cost of reading `/proc` (low milliseconds on a
loaded system).

The feature flag separation ensures a NVIDIA-only user has no
unused AMD code in their binary, and vice versa. A user who
wants both compiles with `--features gpu-nvidia,gpu-amd`.

## Consequences

### Positive

- **One timeline, three samplers.** With the design above, the
  same `ResourceTimeline` ADR-003 introduced now carries CPU
  samples and GPU samples interleaved by `elapsed_ns`. A
  consumer plots them on the same time axis without join
  logic. The shared reference instant ADR-003 chose pays its
  full dividend here.
- **Default build unchanged.** Building inferscope with
  `cargo build --release` produces v0.1.0-equivalent behaviour:
  no NVML link, no GPU sample stream, no surprise dependency on
  driver presence. Users opt in to GPU sampling explicitly.
- **Honest cross-platform story.** The same compiled binary
  works on a CPU-only machine and on a GPU host. The branching
  is at runtime, on driver availability, not at install time on
  hardware detection. A user who clones the repo and builds on
  a laptop runs the same binary that ships to a Lambda
  instance.
- **AMD parity, modest cost.** The parallel `gpu-amd` feature
  closes the NVIDIA-exclusive gap without inventing a new
  abstraction layer. Each vendor's sampler reports the same
  `GpuSample` shape; the consumer treats them uniformly.
- **The `nvml-wrapper` choice keeps unsafe out of `is-sysmon`.**
  The crate's safe API is the entire surface inferscope needs.
  No raw FFI, no manual lifetime management of NVML handles.

### Negative

- **One more dependency.** `nvml-wrapper` adds a transitive
  dependency tree (`nvml-wrapper-sys`, `wrapcenum-derive`).
  This is small and stable, and gated behind a feature flag,
  so users who don't want GPU sampling pay nothing. But the
  feature-on build is one step heavier than the feature-off
  build.
- **AMD path is subprocess-based, not library-based.** Calling
  `rocm-smi` as a child process every tick is conceptually
  uglier than a library call. It also depends on the `rocm-smi`
  binary being on PATH, which is the normal state on AMD-
  configured machines but is one more thing that can go wrong.
  A future revision can replace the subprocess path with a
  bindgen-generated wrapper if a maintained safe wrapper
  appears.
- **Multi-GPU testing surface.** A multi-GPU machine produces N
  samples per tick. The `ResourceTimeline` accepts them, but
  reports and downstream tooling now need to handle the
  device-index dimension. v0.2's CLI report aggregates by
  device; v0.3 may want a per-device sub-report. The added
  dimension is small but real.
- **Sampling cost.** NVML calls are cheap (single-digit
  microseconds each on modern drivers), and reading four fields
  per device fits well within a 50 ms tick. On a 4-GPU machine
  the per-tick cost is dozens of microseconds; on an 8-GPU
  machine it stays in the low hundreds. This is observability
  cost, not zero, but well-bounded.

## Alternatives Considered

### Raw FFI against `libnvidia-ml.so`

Rejected. The full NVML API is large, the safe-wrapper
ergonomics in `nvml-wrapper` are good, and the cost of one
dependency is small compared to the cost of maintaining unsafe
FFI in the long term. A project at the inferscope stage of
maturity gains nothing by reinventing the wrapper.

### A single `Sampler` trait abstracting CPU and GPU

Rejected. The two samplers have different lifetimes, different
initialisation costs, and different error semantics. A
unifying trait would either be so general that it adds no
ergonomic value, or so specific that it leaks GPU concerns
into the `/proc` path. The orchestrator already coordinates
the two samplers concretely; a trait between them would be
abstraction for its own sake.

### Reading GPU data from `/sys/class/drm/`

Rejected. Some GPU metrics are exposed through sysfs nodes,
similar to `/proc` for CPU. The coverage is patchy across
drivers and incomplete for the metrics inferscope needs
(SM utilisation in particular is not in sysfs). Using sysfs
where it works and NVML elsewhere would double the code paths
without adding signal.

### Combining NVML and ROCm into a single feature flag

Rejected. A user on an NVIDIA-only host has no use for the
`rocm-smi` subprocess code or for the `gpu-amd` module. A user
on an AMD-only host has no use for NVML. Forcing them to
compile both, and to ship a binary that links against drivers
they don't have, costs build size and download size for no
benefit.

### Sampling GPU only on token arrival (event-driven)

Rejected for the same reason ADR-003 rejected one-sample-per-
token for `/proc`. Coupling sysmon to probe events loses the
information that lives between tokens. GPU utilisation between
two tokens — whether the device dropped to idle or kept
running at 100 % — is exactly the kind of signal a profiler
exists to capture. The independent-tick design preserves it.

### Deferring AMD to v0.3

Considered, not adopted. Adding AMD support in v0.2 alongside
NVIDIA costs perhaps a few days of additional work over the
NVIDIA-only path, and lands a feature that gives the v0.2
release a complete cross-vendor story. The marginal slowdown
is small; the marginal completeness is large. Deferring AMD
would also leave inferscope with a NVIDIA-only public story
for the article cycle around v0.2, which is narrower than the
project's framing of "LLM inference engines on whatever
hardware they run on" warrants.
