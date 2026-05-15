# ADR-003: sysmon Scope and Temporal Correlation

- **Status**: Accepted
- **Date**: 2026-05-15
- **Deciders**: Michele Campi

## Context

`is-sysmon` samples the inference engine process to record its
resource footprint while a probe run is in progress. Three
decisions need to be settled before any sysmon code is written.

The first is what to read. On Linux, per-process resource state
lives in `/proc/<pid>`. The relevant fields for profiling are
resident set size (memory actually held in RAM), CPU time
consumed (user plus system, expressed in scheduler jiffies), and
thread count. RSS and thread count are in `/proc/<pid>/status`;
CPU time is in `/proc/<pid>/stat`. Both files are plain text and
cheap to read.

The second is how often to sample. Too coarse a rate misses
short-lived spikes during a fast request; too fine a rate adds
load and produces noise that drowns the signal. A reasonable
default needs to bound both error modes.

The third is how to align a sysmon sample with a probe event.
The probe records each token arrival as an offset in nanoseconds
from a reference instant captured at request send time. The
sysmon produces a stream of samples. If a sample says "RSS was
612 MiB" and a token says "I arrived at +412 ms", a consumer
should be able to ask "what was the RSS when the third token
arrived?" without any post-hoc alignment work.

A separate but related question is whether v0.1.0 should also
read GPU state — utilisation, VRAM, thermals — via NVML or
similar. This is a real concern because the engines inferscope
targets in production run on GPUs, and the `/proc` data only
describes the CPU-side process. The question deserves an
explicit answer rather than being silently deferred.

## Decision

Three coordinated choices.

### What sysmon reads in v0.1.0

`is-sysmon` reads three fields from `/proc/<pid>/...` and
produces a structured sample:

  - `rss_bytes` — resident set size in bytes, parsed from the
    `VmRSS:` line of `/proc/<pid>/status` (which is in kibibytes,
    converted to bytes here so the type carries SI-comparable
    values).
  - `cpu_user_jiffies` and `cpu_system_jiffies` — the user-mode
    and kernel-mode CPU time the process has accumulated since
    its start, parsed from fields 14 and 15 of `/proc/<pid>/stat`.
    Stored as raw jiffies; conversion to seconds happens
    downstream when reporting, using `sysconf(_SC_CLK_TCK)`.
  - `thread_count` — the `Threads:` line of `/proc/<pid>/status`.

Each sample carries an `elapsed_ns: u64`: nanoseconds since the
reference instant. The same scheme as ADR-002 is used for the
same reasons.

### Sampling rate

The default sampling period is **50 ms**. The figure is chosen
so that a one-second request yields about twenty samples — enough
to see the shape of RSS and CPU during a single generation —
while the sampling itself stays well under one percent of CPU
on the monitored machine. The rate is a configurable parameter,
not a hard-coded constant, so unusually short or long probe runs
can be tuned.

### Temporal correlation: shared reference instant

The same reference `Instant` is passed both to the probe and to
the sysmon. Both produce `elapsed_ns` values measured from this
shared origin. A consumer correlates a sample with a token by
direct numeric comparison; no post-hoc alignment, no clock skew
to reconcile.

Concretely, the entrypoint that orchestrates a probe run captures
one `Instant`, hands a clone of it to both `is-probe::run` and
`is-sysmon::sample_during`, and the two run concurrently. The
`Instant` itself stays inside whichever crate captured it; only
`u64` offsets cross any boundary, per ADR-002.

### GPU monitoring is explicitly v0.2+

v0.1.0 does not read GPU state. NVML, ROCm SMI, and equivalent
interfaces are not invoked. The architecture leaves room for them
— a future `gpu` module under `is-sysmon` can produce samples in
the same shape with additional fields — but they are not
implemented now.

This is a sequencing decision, not a permanent one. The reasoning
is the same as ADR-001: ship a working CPU-side foundation first
on hardware the project actually runs on (the development VM is
CPU-only), then add GPU support in v0.2 alongside the planned
internal instrumentation work. A v0.1.0 that tried to cover both
CPU and GPU sampling would push the release further out without
producing a stronger artifact in the meantime.

## Consequences

### Positive

- **Single reference instant means trivial correlation.** A
  consumer answers "what was the RSS at the third token?" with
  a binary search over samples by `elapsed_ns`. No timestamp
  munging, no skew correction.
- **Raw jiffies preserved.** Storing CPU time as integer jiffies
  rather than converted seconds keeps the signal lossless, the
  same principle as ADR-002 for token timing. Two consecutive
  samples can be diffed for "CPU work done in this interval"
  without floating-point drift.
- **`/proc` is enough for the CPU-side story.** For local
  llama.cpp, mistral.rs, or Ollama, the engine process's
  `/proc` data tells the full resource story. The tool ships
  useful against the engines that run on the development VM.
- **Configurable rate.** 50 ms is a default, not a floor. A
  user profiling a very short request can drop to 20 ms; a
  long-running batch can rise to 200 ms.
- **GPU door left open.** The sample type and the sampling loop
  are not tied to `/proc`; v0.2 adds a parallel GPU sampler
  whose output joins the same timeline.

### Negative

- **CPU-only story.** Against a vLLM server on an H100, `/proc`
  shows the vLLM process's RSS and CPU but says nothing about
  GPU utilisation or VRAM. The timing portion of inferscope
  still works (it is engine-agnostic), but the resource portion
  is incomplete. This is the limitation the v0.2 GPU work will
  remove; v0.1.0 documents it honestly in the README rather
  than implying coverage it does not have.
- **Sampling adds load.** Reading `/proc/<pid>/status` and
  `/proc/<pid>/stat` every 50 ms is cheap (each is a few hundred
  bytes), but not free. On a heavily loaded system the
  observation itself becomes part of what is observed. The
  effect is small at the chosen rate; users who care can lower
  the frequency or disable sampling for ground-truth runs.
- **Jiffy precision.** CPU time is reported by the kernel in
  scheduler jiffies, typically 100 Hz. Two consecutive samples
  10 ms apart may report identical CPU jiffies because no jiffy
  boundary crossed in between. This is a limitation of the
  data source, not of sysmon; reporting at sub-jiffy resolution
  would invent precision the kernel did not provide.

## Alternatives Considered

### Reading PID-level data from a structured source instead of `/proc`

Rejected. `procfs` crates exist that wrap `/proc` parsing, but
the fields inferscope needs are three plain text values. Pulling
in a dependency to read three numbers from two files would
trade transparency for boilerplate.

### Storing converted SI units instead of raw kernel values

Rejected for the same reason as ADR-002. Storing
`cpu_user_seconds: f64` instead of jiffies discards integer
precision and bakes in a conversion that consumers might want
to do differently. Raw values are the data; conversion is
presentation.

### One sample per token

Rejected. Tying sysmon to probe events would couple the two
crates and lose information between tokens. The current design
keeps them independent — sysmon samples on its own schedule,
the consumer correlates by `elapsed_ns` — and the cost of the
independence is zero.

### Implementing GPU monitoring in v0.1.0

Rejected for scope reasons. NVML or ROCm SMI integration is
straightforward in isolation but expands the v0.1.0 surface
meaningfully: error handling for absent libraries on
CPU-only machines, conditional compilation, platform-specific
testing. None of this is intrinsically hard, but doing it in
v0.1.0 delays the release without making the foundation
stronger. Deferred to v0.2 alongside the engine-internal
instrumentation work from ADR-001.
