# ADR-011: Prometheus vLLM Metric Source for KV-Cache Hit Rate

- **Status**: Accepted
- **Date**: 2026-06-25
- **Deciders**: Michele Campi

## Context

ADR-005 brought GPU sampling in via NVML; ADR-007 split it per device;
ADR-010 integrated power into energy and derived the efficiency family.
ADR-003 established the `/proc` resource loop. Every metric source
inferscope has today is *host-side*: it observes the engine process from
the outside, on the machine where that process runs — `/proc` for CPU and
memory, NVML for the GPU. They share one correlation scheme: each sample
is timestamped `elapsed_ns` from a reference `Instant` the probe also
holds (ADR-003), so any sample joins a token arrival by numeric compare.

These sources cannot see inside the engine. The KV-cache hit rate — the
fraction of prefill token-blocks served from cache rather than recomputed
— is an *application-internal* quantity. It is not visible in `/proc` and
not visible to NVML. vLLM, and the llm-d inference simulators built on its
schema, expose it only through a Prometheus `/metrics` endpoint in the
text-exposition format, as two monotonic counters: `vllm:prefix_cache_hits`
and `vllm:prefix_cache_queries`, labelled by `model_name`. inferscope has
no `vllm:` parser and no HTTP metric source of any kind.

This is a confirmed gap, and it matters beyond convenience. KV-cache hit
rate is one of the two metrics this tool's positioning rests on; the other,
tokens-per-joule, was closed by ADR-010. The llm-d Benchmarking SIG wraps
external harnesses and exposes no metric correlating KV-cache hit rate with
energy on one clock — which is precisely the space a window-correlated
`hit_rate` next to ADR-010's `tokens_per_joule` occupies.

Four decisions need settling: where the adapter lives in the workspace,
how the exposition format is parsed, what granularity the raw data is kept
at, and where the derived hit rate lands in the report schema.

## Decision

### Metric source: a new `is-metrics` crate

The adapter is a third category of metric source, distinct from the two
that exist. `is-sysmon` samples host OS and GPU resources and carries no
HTTP client. `is-probe` drives load against the engine's
`/v1/chat/completions` endpoint — it is the workload generator, not an
observer. Scraping an application's `/metrics` endpoint over HTTP is
neither: it is read-only observation of engine-internal counters across a
network boundary.

Three workspace invariants make the placement forced rather than chosen.
`is-core` is I/O-free and async-free by explicit contract, so an HTTP
adapter cannot live there. `is-sysmon` does not depend on `reqwest` and is
coherent as an OS/GPU resource sampler; folding an HTTP scraper into it
breaks that coherence. `is-probe` does carry `reqwest`, but as a load
driver, not a metric source — a `GET /metrics` poller does not belong with
the `POST` workload path.

`is-metrics` therefore is a new crate. It mirrors the *shape* `is-sysmon`
established rather than inventing one: a free function
`scrape_during(config, start, cancel) -> KvCacheTimeline` that polls at a
configured cadence, timestamps each sample `elapsed_ns` from the shared
reference `Instant` (ADR-003), and is best-effort per tick (a failed
scrape is swallowed; a partial timeline beats an aborted run, per ADR-003
and ADR-006). It pulls `reqwest` from the workspace dependency table,
already pinned with `rustls-tls` per the ADR-010 migration.

### Parser: an internal, targeted parser — not an external crate

Parsing is a pure, I/O-free module inside the crate (`is-metrics/parse.rs`),
mirroring `is-sysmon/parse.rs`, which keeps `/proc` parsing separate from
the sampling loop. It handles exactly the lines that matter: the
text-exposition form `metric{label="value"} number` for the
`vllm:prefix_cache_*` series, skipping `# HELP` / `# TYPE` and any series
not asked for.

This is preferred over a general crate (`prometheus-parse`) for three
reasons. It mirrors the existing house pattern — sysmon parses `/proc`
itself rather than depending on a `/proc` library. It is anchored to a
real input: the Blocco A fixture
`tests/fixtures/llm-d-inference-sim-v0.8.2-metrics.txt` (270 lines, 41
distinct `vllm:` series, prefix-cache populated) is the authoritative test
oracle. And the two series in scope are trivial and stable — counters with
one label — so the general parser's histogram/exemplar handling is dead
weight. The trade-off is stated plainly: if a future need arises to parse
`vllm:` histograms (e.g. `time_to_first_token_seconds_bucket`), the
internal parser must be extended where the crate would already cover it.
That is accepted as YAGNI for the KV-cache objective.

### Raw granularity: a per-tick timeline

The raw layer keeps every scrape as a sample, modelled on `GpuSample`:
`KvCacheSample { elapsed_ns, hits, queries }`, accumulated into a
`KvCacheTimeline { samples, sample_period_ns }`. `hits` and `queries` are
the counters' raw `u64` values, stored in their native integer form at the
data layer, deferring any ratio to the reporting layer — the same
discipline ADR-005 applies to NVML's integer units.

Per-tick rather than baseline-plus-final (the leaner `DeviceEnergy` shape)
because the cache fills over a run, and the curve — how hit rate climbs as
the prefix cache warms — is signal worth keeping. It also matches the
other raw timelines (`GpuTimeline`, `ResourceTimeline`), so the report
schema stays uniform. The cost is a few KB of JSON per run, judged
worthwhile.

### Derived metrics: `KvCacheMetrics` in the report layer

The window hit rate is a derived float and lives in `is-report`, mirroring
`EfficiencyMetrics`. `KvCacheMetrics` carries the window deltas
(`hits_delta`, `queries_delta`) and the derived `hit_rate`
(`hits_delta / queries_delta`), computed from the first and last samples
of the timeline.

Because the inputs are monotonic counters, the window figure is a delta,
not an absolute — the `DeviceEnergy` pattern from ADR-010. This carries one
validity condition that is made explicit rather than assumed: the counter
must not regress within the window. If a later sample shows fewer hits or
queries than an earlier one, the engine was reset mid-run, the delta is
meaningless, and the metric is withheld (`None`) rather than reported
wrong. A warming cache with `queries_delta == 0` likewise yields no rate.

### Report seam

Two optional fields are added to `Report`, both
`#[serde(default, skip_serializing_if = "Option::is_none")]` so reports
written before this ADR deserialise unchanged and a `None` never appears in
output:

- `kvcache_timeline: Option<is_core::KvCacheTimeline>` — the raw band.
- `kvcache: Option<KvCacheMetrics>` — the derived band.

Both are folded in downstream by the orchestrator, at the same stage
`efficiency` is folded, and are `None` when no `/metrics` endpoint is
configured or the `vllm:prefix_cache_*` series are absent from the scrape.

## Consequences

### Positive

- inferscope gains its first application-internal metric, and the one its
  positioning is missing: KV-cache hit rate, on the same `elapsed_ns` clock
  as energy and tokens, so hit rate and tokens-per-joule can be read
  together for one run — the correlation the Benchmarking SIG does not
  expose.
- The new crate isolates the HTTP/exposition concern cleanly; `is-sysmon`
  stays a pure OS/GPU sampler and `is-core` stays I/O-free.
- The per-tick timeline makes the cache-warming curve visible, not just a
  single end-of-run ratio.
- The validity condition on counter regression makes the number safe to
  cite: a reset engine yields no rate, never a wrong one.

### Negative

- A new crate and a new metric source enlarge the build and the surface to
  maintain. Judged proportionate to a portfolio-decisive metric.
- The internal parser covers only the counter series in scope; `vllm:`
  histograms would need parser work the general crate would have supplied.
  Accepted as YAGNI.
- Window-level hit rate shares ADR-010's granularity limit: it is a
  per-run figure, not per-request. Per-request attribution is not a goal
  here.

## Alternatives Considered

### `prometheus-parse` (external crate)

Rejected. It would parse the full exposition format — histograms,
exemplars, `# HELP`/`# TYPE` — when two counter series are needed, and
would break the house pattern of parsing simple text formats internally
(sysmon parses `/proc` itself). The internal parser is ~40 lines against a
real fixture and pulls no new dependency.

### Baseline-plus-final only (the `DeviceEnergy` shape)

Rejected for the raw layer. Keeping only the first and last counter
readings would give the same window `hit_rate` for less JSON, but would
discard the warming curve and break uniformity with the other raw
timelines. The derived metric still uses only first and last; the raw
timeline keeps everything.

### Folding the adapter into `is-sysmon`

Rejected. It would pull `reqwest` into a crate that has stayed free of an
HTTP client, and conflate host-side OS/GPU sampling with cross-network
application scraping. The shared shape (a `*_during` sampling function on
the ADR-003 clock) is a convention to mirror, not a reason to merge.

### Absolute counter values, no delta

Rejected. Reporting `vllm:prefix_cache_hits` as an absolute would fold in
every request served since the engine started, including warm-up before
the probe began. The window delta attributes the hit rate to the probe's
own load, which is what the report is about.
