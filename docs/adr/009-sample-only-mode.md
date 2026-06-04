# ADR-009: Sample-Only Mode and the ResourceReport Type

- **Status**: Accepted
- **Date**: 2026-06-04
- **Accepted**: 2026-06-04
- **Implemented in**: Unreleased (targets v0.4.0)
- **Deciders**: Michele Campi

## Context

inferscope was built as an active profiler: it drives an inference
engine through its OpenAI-compatible API with a prompt of its own
(`--endpoint --model --prompt`), captures per-token timing, and in
parallel samples the engine process (and, per ADR-005/007, its GPUs).
The probe owns the wall clock of the run; the `/proc` sampler and the
GPU sampler are cancelled the moment the probe finishes. The unit of
output is the `Report`: a full profiling run with a `request_timing`
field that is always present.

This model breaks down for a class of measurement that has become
central: profiling a server's resource behavior while an *external*
load generator drives the traffic. The concrete case is the Dynamo
KV-router A/B experiment, where AIPerf replays a production trace
against a multi-worker deployment and we want to know what each GPU
and worker process actually does under each routing mode. There,
inferscope must not generate load of its own — that would perturb the
very thing being measured. It must attach to running PIDs and sample
for a fixed window, contributing nothing to the workload.

The existing `Report` cannot represent this: there is no probe, so no
`request_timing`, no TTFT, no tokens. Forcing it in by making
`request_timing` optional would inject optionality into a field that
every existing consumer (`derive_timing`, `render_json`,
`render_text`, the OTel export) treats as always present, weakening a
type that is otherwise solid, to accommodate a different concept.

## Decision

Add a **sample-only mode** (`--sample-only`) that attaches to a PID
for a fixed `--duration-secs`, samples CPU/RSS (and per-device GPU
when built with `gpu-nvidia` and `--gpu`), and emits a dedicated
**`ResourceReport`** — a separate type, not a degraded `Report`.

- `ResourceReport` carries: `pid`, `include_descendants`,
  `sample_period_ms`, `duration_secs`, `resource: Option<ResourceMetrics>`,
  `gpu: Option<GpuMetrics>`. It reuses the existing derived-metric
  types (`ResourceMetrics`, `GpuMetrics` with its per-device breakdown)
  unchanged — no duplication of the metric schema.
- The sampling primitives (`sample_during`, `sample_gpu_during`) are
  reused verbatim. The only difference from the normal path is the
  cancellation trigger: a `tokio::time::sleep(duration)` timer instead
  of probe completion.
- `--endpoint`, `--model`, `--prompt` become `Option<String>` with
  `required_unless_present = "sample_only"`. The normal path is
  unchanged; clap guarantees they are present there, so the probe
  path unwraps them behind a documented invariant.
- Sample-only always emits JSON: the artifact is meant for the
  analysis pipeline, not human reading.

Each type therefore models exactly one concept: `Report` is a full
profiling run (timing always present); `ResourceReport` is a
standalone resource-sampling window (no timing by construction).

## Consequences

- The normal profiling path is untouched: no regression, no new
  optionality in `Report`, `render_json/text`, or the OTel export.
- A new output type and `render_resource_json` are added to
  `is-report`, exported alongside the existing report API.
- inferscope can now compose with external load generators (AIPerf,
  wrk, k6) instead of only generating its own load. This is what
  enables profiling a distributed system (Dynamo workers) under a
  realistic, externally-driven workload.
- The `gpu` field on `ResourceReport`, like `Report.gpu`, is a plain
  `Option<GpuMetrics>` with no feature gate: the `gpu-nvidia` feature
  lives in `is-sysmon` and the `inferscope` binary, not in
  `is-report`. A caller without the feature passes `None`.
- Future load-driven measurements (latency under sustained load,
  resource behavior during a soak test) fit this type without further
  schema changes.
