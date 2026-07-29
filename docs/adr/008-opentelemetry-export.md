# ADR-008: OpenTelemetry Export of Inferscope Reports

- **Status**: Accepted
- **Date**: 2026-05-25
- **Accepted**: 2026-05-25
- **Implemented in**: v0.4.0 (commits b6e314b..9d0e185 on main, post-v0.3.0)
- **Superseded in part**: see Postscript, 2026-07-29
- **Deciders**: Michele Campi

## Context

ADR-004 established the report contract: inferscope emits a derived
`Report` as either a plain-text summary for terminal reading or a JSON
document for programmatic consumption. Both forms are written to
stdout at the end of a run. The contract works well for the inferscope
CLI in isolation — a developer runs the tool against a local engine,
reads the summary, optionally pipes the JSON to a file.

It is the wrong shape for the operational context most consumers
actually inhabit. Anyone running inferscope inside a continuous
benchmark suite, a CI job, a Kubernetes-managed validation pipeline,
or even an interactive Jupyter notebook has already paid the cost of
running an observability stack (Grafana Tempo, Honeycomb, Datadog
APM, a self-hosted Jaeger). What they want is for the run's per-token
timing, GPU samples, and resource aggregates to land in the trace
view they already use, alongside the spans the engine, the CI runner,
or the orchestrator emits. Today, that requires writing a custom
ingester that parses the inferscope JSON and emits OTLP spans by hand.

The need surfaced concretely during the 24 May 2026 vLLM validation
run on H100. The H100 RunPod environment had a Grafana Cloud Tempo
trace already capturing the engine's request span. To correlate
inferscope's per-token timing with the engine-side request span, a
manual JSON-to-OTLP shim was written ad hoc, used once, and discarded.
Two days later the same need reappeared while writing the
`benchmarks/vllm-vs-llama-cpp.md` case study: the trace view shows
exactly the cold/warm-outlier/warm-steady three-run pattern the
case study describes, but only if the inferscope numbers are spans,
not JSON.

The pattern is similar to ADR-005's GPU sampling decision: the data
is already correct on disk; the gap is the export channel. ADR-005
fixed the channel for the GPU dimension by adding NVML to the
sampler. This ADR fixes the channel for downstream observability
stacks by adding OpenTelemetry as a first-class export target,
alongside text and JSON, behind a feature flag.

The motivating use cases:

- A CI job profiles a model on a self-hosted runner with a Grafana
  Cloud Tempo collector, and wants the run's traces in the same
  dashboard as the rest of the CI fleet.
- A multi-node Kubernetes benchmark Job runs inferscope as a
  sidecar container next to vLLM. Both containers ship spans to
  the cluster's OTel Collector; the operator gets a single trace
  view showing engine internals and inferscope-side timing on the
  same wall clock.
- A developer running inferscope locally points it at a Jaeger
  all-in-one container on port 4318, opens the Jaeger UI, and
  sees per-token arrivals as a timeline on the root span — a
  visualisation the text and JSON reports cannot produce.

## Decision

Add an opt-in `otel-export` Cargo feature on the `is-report` crate.
When enabled, it exposes a single public function:

```rust
pub fn export_to_otel(report: &Report, endpoint: &str)
    -> Result<(), OtelExportError>;
```

The function emits the report as **one root span** named
`inferscope.run`, with:

- **Span kind**: `SpanKind::Client` — the inferscope process acts
  as a client of the inference engine over HTTP.
- **Span attributes**: every derived metric on the report becomes
  an attribute on the root span. Mandatory: `inferscope.endpoint`,
  `inferscope.timing.token_count`, `inferscope.timing.total_latency_ns`.
  Optional (set only when `Some`): `inferscope.timing.ttft_ns`,
  `inferscope.timing.tokens_per_second`,
  `inferscope.timing.inter_token_{p50,p99,max}_ns`,
  `inferscope.resource.{rss_max,rss_mean,thread_max,cpu_mean}_*`,
  `inferscope.gpu.{device_count,vram_max_bytes,utilization_mean,power_mean,temp_max}_*`.
  Cluster-level only on the root span. Per-device GPU metrics
  remain in JSON only for v0.3; ADR-008-followup may add an
  attribute group per device, but only after a real consumer
  asks for it.
- **Span events**: every `TokenArrival` in `report.request_timing.tokens`
  becomes a span event named `token.arrival`, timestamped at
  `run_start + elapsed_ns` so trace UIs (Jaeger, Tempo) render the
  events in temporal order on the span timeline. Event attributes:
  `token.index` (i64), `token.elapsed_ns` (i64).

Implementation choices:

- **Wire protocol: OTLP over HTTP with protobuf payload.** Not gRPC.
  HTTP traverses corporate firewalls without special configuration,
  the dependency footprint is smaller (no `tonic`), and every modern
  OTLP receiver speaks HTTP/protobuf.
- **HTTP client: hyper, not reqwest.** opentelemetry-otlp 0.32 offers
  either as a Cargo feature. `reqwest-client` transitively pulls
  reqwest 0.13, which requires Rust 1.85. The workspace MSRV is 1.83
  (pinned via `rust-toolchain.toml` per ADR-001). `hyper-client` keeps
  the MSRV intact.
- **Span processor: SimpleSpanProcessor (synchronous flush).** Not
  the batching processor. Inferscope is a one-shot CLI: it makes
  one run, builds one report, ends. A background batch worker buys
  nothing and complicates shutdown. `SimpleSpanProcessor` blocks
  until the span has been delivered, which matches the call shape.
- **Resource attributes**: `service.name = "inferscope"`,
  `service.version = env!("CARGO_PKG_VERSION")`. Attached on the
  `Resource` so every span inferscope emits (now or in future) is
  correctly attributed without per-call boilerplate.
- **Failure handling: log and continue.** Export failure does not
  fail the inferscope run. `export_to_otel` returns
  `OtelExportError`; the CLI orchestrator logs the error to stderr
  with the standard `inferscope:` prefix and proceeds to `Ok(())`.
  Observability is secondary to the profiling result.
- **CLI surface**: a new `--otel-endpoint <URL>` flag on the
  `inferscope` binary, with `env = "OTEL_EXPORTER_OTLP_ENDPOINT"`
  so the standard OTel environment variable is honoured. The flag
  is feature-gated: it appears in `--help` only when built with
  `--features otel-export`.
- **Endpoint URL shape**: callers pass the **base URL** of the OTLP
  receiver (e.g. `http://localhost:4318`); opentelemetry-otlp
  appends `/v1/traces` itself per the OTLP/HTTP spec.

The skeleton plus implementation lands across four commits on `main`
post-v0.3.0: feature gate and module skeleton (b6e314b), full
export implementation (48208d3), CLI flag wiring (8b591eb), unit
test of the error path (9d0e185). The feature is shipped to users
in v0.4.0.

## Consequences

**Positive.**

- Any OTLP-capable backend now consumes inferscope runs as
  first-class traces: Jaeger, Grafana Tempo, Honeycomb, Datadog APM,
  New Relic, self-hosted OTel Collectors. No bespoke ingester per
  backend.
- The per-token timeline becomes a directly readable visualisation
  in Jaeger and Tempo. The text and JSON reports could only describe
  the timeline as percentile aggregates; trace UIs render it as a
  span Gantt with one event per token. This is qualitatively new
  information.
- Cross-stack correlation: when both the engine and inferscope
  emit OTLP, the user sees them as sibling spans of the same parent
  trace ID once an upstream context is supplied. Future work may
  thread a parent span ID through the CLI (`--otel-parent-context`)
  to make this automatic.
- Honours the standard `OTEL_EXPORTER_OTLP_ENDPOINT` environment
  variable, so the tool fits into existing OTel deployments
  without extra configuration.

**Negative.**

- Roughly nine additional transitive crates compile when the feature
  is enabled (`opentelemetry`, `opentelemetry_sdk`,
  `opentelemetry-otlp`, `opentelemetry-http`, `opentelemetry-proto`,
  `hyper`, `hyper-util`, `prost`, `prost-derive`, plus their proc-macro
  dependencies). Cold compile of the feature-enabled build adds
  roughly 90 seconds on the CI runner. The default build is
  untouched.
- The HTTP client requires an active tokio runtime context. The
  inferscope binary already runs inside `Runtime::block_on`, so this
  is invisible at the CLI level, but it surfaces as a constraint for
  any future library consumer of `is_report::export_to_otel`. The
  function's doc comment makes the requirement explicit.
- Per-device GPU metrics are flattened to cluster-level on the root
  span. The JSON output (ADR-007) still carries the full per-device
  breakdown; users who need per-device data in their trace view
  must read the JSON for now. The shape is straightforwardly
  extensible — one child span per device, or one attribute group
  per device — but extending it without a real consumer driving
  the design is speculative work.

**Neutral.**

- The feature is opt-in. The default `cargo build` and `cargo install
  inferscope` produce a binary with no OTel dependency surface, no
  `--otel-endpoint` flag visible in `--help`, and no exported
  function in `is_report`'s public API. Distros, hobbyists, and
  CI matrices that don't need OpenTelemetry pay zero cost.
- The Docker image published by the GitHub Action on tag push
  currently builds without the feature, matching the `cargo install`
  default. A separate image variant (`ghcr.io/michelecampi/inferscope:0.4.0-otel`)
  may follow in v0.4.0; the decision is deferred until there is a
  user asking for it.

## Alternatives Considered

**OTLP/gRPC instead of OTLP/HTTP.** gRPC is the more performant
transport and the OTel Collector defaults to listening on both ports
(4317 gRPC / 4318 HTTP). It was rejected because the `tonic` gRPC
client used by opentelemetry-otlp 0.32 with the `grpc-tonic` feature
requires Rust 1.85, the same MSRV problem reqwest 0.13 has. Even
absent the MSRV constraint, gRPC payload size and throughput
advantages are immaterial for a one-shot CLI emitting a single
span per run. HTTP/protobuf with hyper is the right floor for the
use case.

**reqwest-client instead of hyper-client.** The reqwest path is the
opentelemetry-otlp default and the one most documentation examples
use. Rejected because of the reqwest 0.13 → Rust 1.85 MSRV bump
discussed above. The is-probe crate continues to use reqwest 0.12
for its inference-engine HTTP client; the two paths do not share a
transitive dep because `is-report` does not depend on reqwest at all
in the hyper-client configuration.

**Multiple child spans per token instead of span events.** Initially
attractive because Jaeger renders child spans as separate timeline
bars, which is more visually striking than events on the parent
span. Rejected because it is the wrong OTel-semantic shape: a token
arrival is a **timestamp**, not a **sub-operation with duration**.
A span exists when the work it represents has a start and an end;
a token has only an arrival. Modelling each token as a degenerate
zero-duration span would produce a thousand spans for a run of a
thousand tokens, inflating trace size by three orders of magnitude
for no information gain over the events-on-parent representation.
This also makes ingestion costs explode on metered SaaS backends
like Honeycomb and Datadog.

**OpenTelemetry metrics in addition to traces.** The OTel data
model has a separate Metrics signal (gauges, counters, histograms)
that maps naturally onto inferscope's derived aggregates: VRAM peak
as a gauge, tokens emitted as a counter, inter-token latency as a
histogram. Rejected for v0.4. The Metrics SDK in opentelemetry-rust
0.32 is less mature than the Tracing SDK, adds another ~6 transitive
crates, and provides no information the trace attributes do not
already carry for the one-shot CLI case. If continuous-profiling
emerges as a real use case (inferscope running periodically against
a long-lived engine and emitting metrics on a cadence), revisit then.

**OTLP/JSON instead of OTLP/protobuf.** OTLP/HTTP supports two
payload encodings: protobuf (binary, default) and JSON (text).
Rejected because the JSON encoding is documented as less mature
than protobuf and not all collectors implement it. Protobuf is
the safe default for any conformant OTLP/HTTP receiver.

**Implement OTLP wire format by hand instead of using
opentelemetry-otlp.** Theoretically possible — the OTLP/HTTP spec
is public and the protobuf schema is generated — and would
eliminate the nine-crate dependency surface. Rejected because the
maintenance cost of tracking spec changes, retry semantics, and
the OTLP/HTTP framing details is unjustified for a feature that
mostly forwards already-computed data. The opentelemetry-rust
project exists precisely to absorb that cost.

**Emit OTLP from a separate `is-otel` crate.** Considered placing
the export in a new sixth workspace crate to keep `is-report`
free of OTel dependencies even as an optional feature. Rejected
because OTel is a presentation-layer concern (alongside text and
JSON rendering), and the feature gate keeps the dependency surface
hidden from default builds. Adding a sixth crate for one function
is over-fragmentation.

## Postscript — 2026-07-29

Three statements above were true when written and are not true now. They are
left in place, because an ADR records a decision as it was taken; what changed
is recorded here.

**The feature shipped broken.** "The feature is shipped to users in v0.4.0" was
the intent. In fact `otel.rs` stopped compiling under its own feature on
2026-07-18, when the ADR-013 wall-clock anchor moved two bindings into a match
branch that the span's end timestamp still referred to from outside, and the
module's sample report was never given the six fields ADR-011, ADR-012 and
ADR-013 had added to `Report`. The v0.4.0 tag, cut 2026-07-25, contains that
code. No CI job built with `--all-features`, so nothing reported it. Fixed
2026-07-29; CI gained an `all-features` job in the same session.

**The MSRV argument no longer holds.** `hyper-client` was chosen over
`reqwest-client`, and HTTP over gRPC, because both alternatives required Rust
1.85 against a workspace MSRV of 1.83. The MSRV moved to 1.85 on 2026-06-21
(`6ca675f`), when reqwest migrated to rustls-tls and rustls 0.23 via
hyper-rustls required it. The decision stands on its remaining grounds — a
smaller dependency footprint and no `tonic` — but the constraint that drove it
is gone, and a future revisit should not treat the MSRV as an obstacle.

**Exported attributes stop at ADR-010.** "Cluster-level only on the root span"
and "remain in JSON only for v0.3" describe a report that has since gained
KV-cache metrics (ADR-011), per-phase energy attribution (ADR-012) and
per-step trajectory attribution (ADR-013). None of them reach the trace. This
is a gap in this ADR's contract rather than a regression: the attribute list
here is closed and was written in May. Extending it is deliberate design work,
and `trajectory` in particular is per-step and has duration — the case this
ADR resolved in favour of events precisely because a token arrival does not.
