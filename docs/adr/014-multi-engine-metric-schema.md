# ADR-014: Multi-engine metric schema (vLLM + SGLang)

- **Status**: Accepted, implemented
- **Date**: 2026-07-26
- **Implemented in**: v0.5.0 (b446c7e design; ea2891c D4 amendment;
  39850b9 provenance; 04788c4 schema and summed numerator; 004db34
  per-phase Option; c94dc99 report provenance and schema version;
  529928e the `--engine` flag)
- **Deciders**: Michele Campi

## Context

Every measurement inferscope has published was taken against vLLM. ADR-011
introduced the Prometheus scrape source with `vllm:`-prefixed series names as
compile-time constants; ADR-012 built per-phase energy attribution on the
per-phase timing counters that vLLM exposes; ADR-013 joined per-trajectory
attribution on `vllm:generation_tokens_total`. The engine vocabulary is wired
through three ADRs and four crates.

SGLang is the second engine worth measuring, for one reason that is not
fashion: it is designed around prefix reuse, which is exactly the regime where
the agentic-kv campaign measured a 69.2% swing in tokens/joule. A comparative
measurement on an agentic workload is the intended endpoint. This ADR is the
design prerequisite; it commits no code and no GPU time.

The naive shape of that change — "make the metric names configurable" — is
wrong, and the reason it is wrong is specific rather than stylistic. See D1.

### What the scrape actually exposes (verified at source, not assumed)

**SGLang**, `python/sglang/srt/observability/metrics_collector.py` on `main`
(2209 lines; the file moved from `srt/metrics/`, so older references are stale):

- The metric prefix is `sglang:`, with the colon. Third-party write-ups
  claiming a move to `sglang_` in v0.5.4 are contradicted by the source; the
  official documentation is correct.
- `sglang:generation_tokens_total` — Counter, generation tokens processed.
  Exact analogue of `vllm:generation_tokens_total`. The ADR-013 tokens/joule
  join carries over unchanged.
- `sglang:prompt_tokens_total` — Counter, prefill tokens processed.
- `sglang:cached_tokens_total` — Counter, cached prompt tokens, carrying an
  extra `cache_source` label (device/host/storage). Must be summed across
  label values. See D4.
- `sglang:cache_hit_rate` — Gauge with `multiprocess_mode="mostrecent"`. Not
  usable for window arithmetic and deliberately ignored: the hit rate is built
  as Δ`cached_tokens_total` / Δ`prompt_tokens_total`, the same delta-counter
  form as ADR-011.
- Line ~1699 computes `prompt_tokens - cached_tokens`, establishing that cached
  is a subset of prompt and the ratio is bounded in [0,1].
- There is **no** analogue of `request_prefill_time_seconds_sum` /
  `request_decode_time_seconds_sum`. What exists is
  `sglang:time_to_first_token_seconds`, `sglang:inter_token_latency_seconds`,
  `sglang:e2e_request_latency_seconds`, `sglang:queue_time_seconds`, and
  `sglang:forward_execution_seconds_total`. The first four are per-request
  wall-clock figures that include queueing time and therefore do not decompose
  into engine-side phase occupancy. The last is a single aggregate that is not
  split by phase and therefore cannot be apportioned between prefill and
  decode. None is a substitute. See D3.
- `page_size` is declared in `server_args.py:863` as `Optional[int] = None`
  behind `--page-size`, and resolved by
  `arg_groups/overrides.py::_page_size_default` to **1** on non-HIP and
  non-MUSA platforms — so 1 on A10 and H100 — and to 64 on HIP with
  vectorized_5d and on MUSA.
- The attention backend selection is conditioned on page size (FA3 on Hopper
  requires `page_size > 1`; some paths are `page_size = 1` only). Changing the
  page size changes the kernel path. This constrains the experiment design.

**vLLM**, read in `/root/vllm-src` (`main`, grafted — line numbers differ from
the `/root/vllm-021` worktree at tag v0.21.0; if the experiment pins a version,
re-verify against that tag):

- `vllm/v1/metrics/stats.py`, `class PrefixCacheStats`: `queries` is documented
  as the number of tokens queried, and `record()` performs
  `self.queries += num_tokens` and `self.hits += num_hits`. The denominator is
  **exact tokens**, not blocks.
- `vllm/v1/core/kv_cache_manager.py` (~200-231) is the only relevant call site.
  It passes `num_tokens = request.num_tokens` (exact) and
  `num_hits = num_new_computed_tokens`, obtained from `find_longest_cache_hit`
  under `max_cache_hit_length = request.num_tokens - 1` — the final token must
  be recomputed to produce logits — with an explicit source comment that this
  can force recomputation of a whole block, because `allocate_slots()` requires
  `num_computed_tokens` to be block-aligned. The numerator is therefore
  **quantized to block boundaries**.

Source verification for this ADR was re-run against a local clone at commit
`8a311d1c` (2026-07-27). Two facts recorded after the first draft: the label
dictionary is built with `model_name` taken from
`server_args.served_model_name`, so the model selection key is identical on
both engines and does not belong in `EngineSchema`; and `cache_source`
carries a reserved `total` value that is not part of the partition — see D4.

### The resulting asymmetry

|              | vLLM                                  | SGLang (default)      |
|--------------|---------------------------------------|-----------------------|
| denominator  | exact tokens                          | exact tokens          |
| numerator    | block-aligned (default block size 16) | exact tokens (page_size = 1) |
| bias         | underestimate, monodirectional        | ~0                    |

The two engines' hit rates are not the same quantity. The difference is small
but systematic: it does not average out across requests, and it always points
the same way. It is also analytically computable rather than something to be
estimated. See D5.

### Correction carried by this ADR

ADR-011's Context describes the hit rate as a fraction of prefill
*token-blocks*. The vLLM source says tokens. That description was copied
downstream into type documentation and into rendered report output, and is
corrected in the same commit as this ADR. The published article corpus is
unaffected: the only blog occurrence of block-denominated cache accounting
describes llm-d's prefix-based decider, which genuinely does compute cached
blocks times block size, and is correct as written.

## Decision

### D1 — `EngineSchema` is a type, not configuration

Engine differences are described by an `EngineSchema` that declares, per
engine: the series backing each measurement role, the label aggregation each
series requires, and which capabilities the engine supports at all. Metric
names remain compile-time constants; the schema selects among them.

The decisive argument is a concrete collision, not a preference for types over
strings. inferscope's parser holds `METRIC_QUERIES` and `METRIC_PROMPT_TOKENS`
as distinct roles, and on vLLM they are backed by two distinct series
(`vllm:prefix_cache_queries` and `vllm:prompt_tokens_total`). On SGLang they
collapse onto **one** series, `sglang:prompt_tokens_total`, which serves as
both the hit-rate denominator and the prefill token count. A name map cannot
express "these two roles are the same series here and different series there"
without silently pointing two keys at one series and inviting double counting
downstream. The abstraction belongs at the level of roles and their backing
series, not at the level of names.

### D2 — Provenance travels to the report; the ratio is never silently unified

`parse_kvcache` keeps its `(numerator, denominator)` shape. What changes is
that the semantic provenance of the numerator travels with the value all the
way to the report, and is rendered. A hit rate measured under block-aligned
accounting and one measured under exact-token accounting are never presented
as the same figure without the distinction visible.

This is not hygiene. `is-report/src/render.rs` printed the rate as
`({} / {} token-blocks)` until the correction carried by this ADR — already
the wrong unit on vLLM, and one that would become engine-dependent:
block-aligned tokens on vLLM, exact tokens on SGLang at `page_size = 1`.
The rendering layer needs the provenance in order not to state a false
unit. The JSON fields (`hits_delta`,
`queries_delta`, `hit_rate`) are unit-neutral and unchanged, so archived
evidence under `validation-results/` remains valid; the CHANGELOG must record
this as a label correction at unchanged metric, so that old and new rendered
output are not mistaken for a change of measurement.

### D3 — Per-phase timing becomes `Option`; absence is not zero

The per-phase timing counters ADR-012 depends on do not exist on SGLang. They
become `Option` at the parse boundary and propagate as `None`, never as zero —
the same move already made for the operator's placement signals, and for the
same reason: a serde default of 0.0 fabricates a measurement that was never
taken.

`sglang:forward_execution_seconds_total` is explicitly considered and rejected:
it is a single aggregate not separated by phase, so it cannot be apportioned.
The per-request latency families are rejected because they are wall-clock and
include queueing.

The consequence is stated in full under Consequences: on SGLang the time-share
leg of ADR-012's dual apportionment is absent, so the token-share leg loses its
cross-check and becomes unfalsifiable within the tool.

### D4 — `cached_tokens_total` is summed across `cache_source`, except `total`

The hit-rate numerator on SGLang is the sum of `sglang:cached_tokens_total`
over the values of the `cache_source` label that partition the count — that
is, every value except the reserved literal `total`.

The unqualified form of this rule, "sum over all values of `cache_source`",
was wrong, and the correction comes from the emitter rather than from the
metric declaration. `metrics_collector.py` reports cached tokens through two
mutually exclusive branches selected per request by the optional
`cached_tokens_details` argument: the detailed branch emits `device`, `host`
and `storage_<backend>`, which partition the count; the compatibility
fallback emits a single `cache_source="total"` carrying the same quantity in
aggregate. Because the series is a cumulative counter and the branch is
chosen per request rather than per server, one scrape body can carry both
families at once, and summing them counts the fallback requests twice.

Excluding `total` is preferred over whitelisting the partition values
because the storage label is constructed dynamically as `storage_<backend>`,
with `unknown` as its fallback: a whitelist would silently omit tokens
served from an unanticipated backend, understating the hit rate. `total` is
the only reserved literal, and it appears exactly once in the emitter.

A body carrying only `total` yields an empty sum. That is a detectable
condition, not a zero: the numerator is unavailable under this rule and is
reported as absent rather than as no cache hits.

`extract_label` already handles multi-label series; the schema declares the
aggregation so that the parser does not silently pick one label value. A
per-source breakdown is out of scope here and is not blocked by this shape.

Note that SGLang's own `benchmark/one_batch_server.py` accumulates every
line matching the metric prefix, `total` included, and so carries the double
count this decision avoids.

### D5 — The block-quantization bias is part of the measurement contract

The vLLM hit rate carries its own upper bound on underestimation, reported
alongside the figure rather than mentioned in prose.

The bound is aggregate, because the hit rate is a ratio of sums over a window
and not a mean of per-request ratios. Its form is

    bound = (requests_with_a_hit x block_size) / sum(prompt_tokens)

Requests with no cache hit contribute no bias. The bound is monodirectional:
the measured vLLM rate is never above the exact-token rate. For illustration,
a workload of 1024-token prompts at block size 16 in which every request hits
gives a bound of 16/1024 = 1.56%.

Both factors are known offline: the agentic-kv harness generates its workload
deterministically from a sha256 seed, so the prompt-length distribution is
reproducible without a GPU. The bound is therefore computed by a command that
regenerates it from versioned inputs, not declared as a number.

### D6 — Engine selection is explicit and mandatory

The active schema is chosen by an explicit `--engine` argument, required
whenever a metrics endpoint is scraped, with no default.

Sniffing the metric prefix out of the response body is rejected as the primary
mechanism: a body matching neither vocabulary would yield no series and, under
any tolerant parse, zeros — absence written as zero, which is the failure mode
that has already produced wrong-but-plausible evidence in this portfolio. If
auto-detection is added later it is a cross-check that errors on mismatch with
the declared engine, never a fallback that guesses.

### D7 — Report schema gains a provenance field under an explicit version

The provenance carried by D2 is a new field on the report. The report is
consumed by the ADR-013 `--steps-file` join and by the agentic-kv harness, so
this is a schema change: the report carries a schema version, the new field is
optional on read so that reports written before this change still parse, and
absence of the field is read as "vLLM block-aligned accounting" only when the
report's version predates this ADR. Reports cited by published articles are
readable without rewriting.

## Consequences

### Positive

- A second engine can be measured without forking the measurement path, and
  the two results are comparable with their difference stated rather than
  assumed away.
- The tokens/joule join of ADR-013 carries over unchanged: both engines expose
  a generation-token counter with the same semantics.
- The block-quantization bias becomes a reported, regenerable quantity. It was
  present and unreported in every vLLM hit rate published so far.
- Three documentation sites and one rendering site that stated a wrong unit are
  corrected, and the type documentation stops contradicting the source.
- The `Option` treatment of per-phase timing removes the last place where a
  serde default could fabricate a zero measurement.

### Negative

- On SGLang, ADR-012's dual apportionment degrades to a single leg. The
  divergence between time-share and token-share was the signal that made the
  attribution falsifiable from inside the tool; without it, the token-share
  figure on SGLang is an assertion the tool cannot check against itself. Any
  cross-engine claim about phase attribution must therefore be framed as
  vLLM-only, or validated by an instrument outside inferscope.
- `EngineSchema` adds a type parameter to a parsing path that was previously
  monomorphic, and every capability that is `Option` on one engine widens the
  report's null surface.
- The comparison inherits SGLang's page-size coupling: the default
  `page_size = 1` that makes its hit rate exact also constrains which attention
  backends are eligible. The engines are compared at their respective defaults,
  which is a deliberate choice and a limitation.

## Validation boundary (VM vs GPU)

On the VM, at zero cost: parser and schema selection against recorded
exposition-format fixtures for both vocabularies; the SGLang fixture is
transcribed from the source definitions listed above, including a multi-valued
`cache_source` label to exercise D4; the `Option` propagation of absent
per-phase timing; the D6 failure mode, asserting that a body of the wrong
vocabulary is an error and not a zero; the D5 bound computed from the
harness's deterministic workload.

Requiring GPU: a live scrape against a running SGLang server, and every
comparative number. Whether SGLang offers a CPU-only or simulated serving mode
that would allow a live scrape on the VM — the role `llm-d-inference-sim`
played for the vLLM schema in ADR-011 — is **not verified** and must be checked
before any session is scheduled, not assumed at the node.

The lesson from the ADR-013 session applies directly: that runbook broke twice
on flags never exercised outside the GPU path. Any engine-selection flag
introduced here is exercised in rehearsal, not first at the node.

## Alternatives Considered

**Metric names as configuration.** Rejected per D1: cannot express role
collapsing, and pushes an engine-shape decision into a config file where it
cannot be type-checked.

**Generalize to a third engine (TensorRT-LLM) now.** Rejected. The abstraction
would not be earned by a second implementation but guessed from a third, and
no workload exists that would exercise it. `EngineSchema`'s shape does not
preclude a third engine; this ADR simply does not add one.

**Align the engines by running vLLM and SGLang at a common page/block size.**
Rejected as the primary arm. On SGLang, page size selects the attention backend
kernel path, so aligning the cache accounting perturbs throughput and energy —
the axes the comparison exists to measure. The primary arm runs both engines at
their production defaults with the vLLM bias declared and bounded. A secondary
arm at `--page-size 16` remains a nice-to-have if GPU budget allows, scoped to
the hit rate alone, with throughput and energy from that arm declared
non-comparable. Note what such an arm would establish: it measures SGLang's
analogue of the block-quantization mechanism, not vLLM's bias. The relation is
analogy, not transfer.

**Use `sglang:cache_hit_rate` directly.** Rejected: a `mostrecent`
multiprocess-mode gauge is a point observation with no defined behaviour under
window differencing, and would silently break the delta-counter contract that
ADR-011 established for exactly this reason.
