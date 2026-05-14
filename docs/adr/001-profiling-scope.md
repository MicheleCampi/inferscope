# ADR-001: Profiling Scope for v0.1.0

- **Status**: Accepted
- **Date**: 2026-05-14
- **Deciders**: Michele Campi

## Context

inferscope is a profiling and observability tool for LLM inference
engines. The space of "profiling an inference engine" is wide, and a
v0.1.0 that tries to cover all of it would not ship. A scope decision
is needed before any measurement code is written.

There are two broadly different ways for a tool to observe an
inference engine.

The first is **API-level profiling**. The tool treats the engine as a
black box and drives it through its HTTP API, which for the current
generation of engines (llama.cpp server, mistral.rs, vLLM, Ollama,
TGI) is OpenAI-compatible. From the outside the tool can still measure
a great deal: time-to-first-token, the inter-token latency
distribution, total request latency, and tokens-per-second derived
from token arrival timestamps. It can correlate this with the engine
process resource footprint (resident memory, CPU utilization, thread
count) sampled from the operating system. This requires no
cooperation from the engine and works identically across every
engine that speaks the OpenAI API.

The second is **engine-internal instrumentation**. The tool hooks
into the engine's internals to attribute time and memory to specific
phases: KV cache management, attention computation, the sampling
loop, memory allocation per tensor. This is far more informative but
requires either patching a specific engine or attaching
system-level tracing (eBPF, perf) and correlating by hand. It is
engine-specific work and significantly larger in scope.

A v0.1.0 has to pick one as its foundation.

## Decision

inferscope v0.1.0 is built on **API-level profiling**.

The tool drives an inference engine through its OpenAI-compatible
HTTP endpoint and measures, per request: time-to-first-token,
inter-token latency and its distribution, total latency, and
tokens-per-second derived from token arrival timestamps. It
correlates these with the engine process resource footprint
(resident set size, CPU utilization, thread count) sampled from
`/proc`.

Engine-internal instrumentation is explicitly **out of scope for
v0.1.0** and recorded on the roadmap as the direction for v0.2 and
beyond.

This is a foundation-first decision, not a ceiling. v0.1.0 ships a
complete, useful tool; later versions add depth on top of it.

## Consequences

### Positive

- **It ships.** API-level profiling is achievable in the planned
  v0.1.0 window. There is no open-ended research risk.
- **Engine-agnostic.** Because every current engine speaks the
  OpenAI API, the same tool works against llama.cpp, mistral.rs,
  vLLM, Ollama, and TGI without per-engine code.
- **No benchmark arms race.** The tool's value is diagnostic
  ("here is where your latency goes, here is how your resource
  use behaves"), not a throughput score that could lose to
  llama.cpp on given hardware. The value does not depend on
  winning a number.
- **Clean growth path.** Internal instrumentation in v0.2+ adds
  to the same report model rather than replacing it. The crate
  boundaries (probe, sysmon, report) do not change shape.

### Negative

- **Limited attribution depth.** API-level profiling can say a
  request spent 400ms before the first token, but not how much
  of that was KV cache setup versus prompt evaluation. Phase-level
  attribution waits for v0.2.
- **Sampling granularity.** Resource sampling from `/proc` has a
  resolution floor; very short requests are characterized by
  fewer samples. The report must represent this honestly rather
  than implying false precision.
- **"Profiler" framing.** The black-box approach is less
  immediately impressive than internals work. This is accepted:
  a shipped, useful tool with an honest roadmap is worth more
  than an ambitious one that stalls.

## Alternatives Considered

### Engine-internal instrumentation as the v0.1.0 foundation

Rejected for v0.1.0 on scope and risk grounds, not on merit. It
is the more informative approach and is the explicit v0.2+
direction. As a starting point it carries open-ended risk:
patching a specific engine or correlating system-level traces by
hand is engine-specific and hard to bound to a release window.
Building it first risks a v0.1.0 that is half-finished in week
six. The chosen sequence — ship API-level profiling, then add
internal depth — de-risks the project and produces a public
artifact earlier.

### A single-engine deep profiler

Rejected. Picking one engine (say, llama.cpp) and instrumenting
it deeply would narrow the audience to that engine's users and
tie the tool's relevance to that engine's internals. API-level
profiling reaches every OpenAI-compatible engine at once, which
is the entire current field.
