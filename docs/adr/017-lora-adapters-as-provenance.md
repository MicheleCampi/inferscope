# ADR-017: Active LoRA Adapters as Report Provenance

- **Status**: Accepted
- **Validation as of 2026-09-21, when this ADR was written**: none. Nothing
  here is implemented. The premise was reproduced against
  llm-d-inference-sim at `e924683`, with the script that ships in the
  lora-multitenancy-experiment repository.
- **Deciders**: Michele Campi

## Context

A campaign comparing cost per token across cells that differ only in how
many LoRA adapters serve the same work needs each report to say which
adapters were active. Without that, a tokens-per-joule figure taken with
eight adapters and one taken with a single adapter are different numbers
presented as the same.

vLLM exposes the active set as `vllm:lora_requests_info`. The Gauge exists
only when `lora_config` is not `None` (`vllm/v1/metrics/loggers.py:982`), its
value is a timestamp (`set_to_current_time()`, line 1111), and the data is in
the labels: `running_lora_adapters` and `waiting_lora_adapters`, each a
comma-separated list of names inside one label value, alongside `max_lora`.

inferscope does not read it today. `EngineSchema` has no LoRA role, and every
`Aggregation` variant reduces the lines of a family to one numeric value,
which this family does not have. The canonical fixture already carries the
line, at lines 65-67 of
`crates/is-metrics/tests/fixtures/llm-d-inference-sim-v0.8.2-metrics.txt`;
the parser meets it on every test run and discards it, because no schema
series matches its name.

Reading it at all required a prior fix. Until `05faba1`, `extract_label`
split the label block on `,` before looking for `=`, so a value holding a
comma aborted the search for every later label. That is exactly the shape of
`running_lora_adapters` once two adapters are concurrent.

## Decision

### D1: Not a timeline

The three raw timelines, KV-cache, phase and speculative, are series of
monotonic counters, and every derivation takes deltas across the window.
A set of adapter names has no delta. The stronger reason is a rule this
codebase already follows: when a sample type cannot express a
distinction, the scrape produces nothing rather than an approximation
(`crates/is-metrics/src/scrape.rs:89-92`). A `LoraSample` holding names
cannot express load per adapter. Building one would write into inferscope
the loss that vLLM and llm-d-inference-sim commit when they reduce a
per-adapter count to a list of names, and that llm-d-router inherits: it
holds a `map[string]int` with room for the count and fills it with zeros,
because the names are all that reach it.

### D2: Not an `EngineSchema` role, in the schema's present form

Two objections, and they carry different weight.

ADR-014 D1 defines the schema as the series backing each measurement
role, the label aggregation each requires, and which capabilities an
engine supports. A LoRA role is not excluded by that definition.

It is excluded by the shape of the types. `Series` is a name and an
aggregation, and every `Aggregation` variant answers how lines combine
into one value. Adding a categorical kind would extend the schema rather
than use it. That extension is deferred here, not rejected. ADR-014 D4
said of per-source cache breakdowns that one "is out of scope here and is
not blocked by this shape", and the same holds for a categorical kind.

### D3: It is provenance, in the sense of ADR-014 D2

ADR-014 D2 carries the hit-rate accounting to the report because, without
it, a block-aligned rate and an exact-token rate are different figures
presented as one. The criterion is not specific to accounting: a fact
travels with a measurement when the measurement would be misread without
it.

The active adapter set meets that criterion, and meets it for every
measurement in a `--sample-only` report rather than for one. It is
recorded as provenance of the window, not as a metric of the engine.

### D4: Typed so that absence can be read

A bare `Option<Vec<String>>` would collapse three cases onto `None`: LoRA
not configured, so the Gauge does not exist; a build that could have
recorded the set and did not; and a report written before this ADR.
`HitRateProvenance` in `crates/is-report/src/metrics.rs` already separates
the equivalent cases for hit-rate accounting, and its resolution is
"deliberately not a default": a recorded fact always wins, an absence on
an unversioned report is an inference, an absence on a versioned one
asserts nothing. The adapter set follows the same discipline.

`max_lora` is optional within a recorded set. vLLM's Rust frontend builds
the label set from the two adapter lists alone, with no `max_lora`
(`rust/src/engine-core-client/src/metrics.rs`, test expectations at lines
455-469), so its absence there is a property of the producer, not a
failed read.

### D5: Its own read, and the reader returns raw sets

The adapter set is read with its own GET, not taken from the body the
KV-cache loop already fetched. ADR-016 D2 made the same choice for the
speculative loop, for a reason that carries over: folding reads together
couples failure modes that are unrelated and puts the established KV path
at risk to save one request against a localhost endpoint.

The reader returns the two label values as they were read, absences
included, and the decision about what an absence means is made by the
provenance type. ADR-016 D3 placed that decision in `is-core` rather than
in the I/O layer, so that the scrape does not settle a question the type
already settles.

### D6: One read, when the window closes, and a new schema version

The set is read once, as the sampling window closes, and recorded as a
confirmation of the configuration the load generator declared rather than
as a measurement of what happened during the window. In the campaign this
ADR serves, the distribution across adapters is fixed in advance by the
list passed to vLLM's serving benchmark; what the report needs is evidence
that the server saw that configuration.

A successful read that finds no `vllm:lora_requests_info` records that
LoRA was not configured, since the Gauge exists only when LoRA is. A read
that fails records nothing, and says so. These are different outcomes and
the type keeps them apart.

`REPORT_SCHEMA_VERSION` is raised. The ADR-016 postscript left it
unchanged for two optional KV-cache fields, on the ground that an absent
`kvcache_timeline` has one meaning, not two. That ground does not hold
here: this field's type resolves absence against the version, as
`HitRateProvenance` does, and without a new version a report written
before this ADR cannot be told apart from one that simply saw no LoRA.

### D7: What this does not claim

It does not measure load per adapter. That is the quantity every
producer in the chain drops, and a reader downstream of them cannot
recover it.

It does not describe the window. The active set changes as requests
overlap; a single read at the close sees the last state and misses
adapters that were active only earlier. It confirms a declared
configuration and cannot detect traffic that diverged from it.

It covers `ResourceReport` only. `metrics::Report`, the probe-path report,
was not read in full before this decision and is left unchanged.

It makes no claim about cost. It records which adapters were serving;
what serving them costs is for the campaign to measure.

## Consequences

### Positive

- A report from any cell of the campaign carries its own provenance and
  can be read without the load generator's log beside it.
- No new abstraction. The reading uses `extract_label`, and the type
  follows the discipline `HitRateProvenance` already set.
- The KV-cache path is untouched: the read has its own request and its
  own failure mode.

### Negative

- The schema version rises, and every reader of `schema_version` must be
  checked before the change ships. That audit is not done here.
  `HitRateProvenance::resolve` is unaffected as read, since it treats any
  present version alike (`(None, Some(_)) => Self::Unknown`); the other
  readers were not read before this decision.
- One more request per `--sample-only` run.
- A single read at the close of the window cannot see adapters that were
  active only earlier in it, as D7 states.

## Alternatives Considered

### A LoRA timeline

Rejected, D1. The three existing timelines share one property, that they
are series of counters with a delta across the window, and a set of names
does not have it. More decisively, a sample holding names would record
the very loss the chain commits.

### A categorical `Aggregation` variant

Deferred, D2. It is compatible with ADR-014 D1 and would make LoRA a
schema role. It is not done here because it changes what `Aggregation`
means for every existing role, and the campaign does not need it.

### Reading the adapter set from the KV-cache loop's body

Rejected, D5, for the reason ADR-016 D2 gives: it couples unrelated
failure modes to save one request against a localhost endpoint.

### No field: rely on the load generator's declared list

Rejected, D3. A report separated from its run log could not be
interpreted, which is the condition ADR-014 D2 exists to prevent. The
declared list is what the recorded set confirms; it is not a substitute
for recording it.
