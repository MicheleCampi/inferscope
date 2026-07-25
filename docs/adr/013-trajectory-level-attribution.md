# ADR-013: Trajectory-Level Energy and Cache Attribution for Agentic Workloads

- **Status**: Accepted
- **Validation**: measured on GPU 2026-07-21 (1x A10, vLLM, Qwen2.5-7B-Instruct) — per-step energy with exact reconciliation, evidence in `validation-results/adr-013-a10-vllm/`. That evidence also exposed the delta-baseline flaw corrected below; per-step counter figures predate the correction and are recomputed by the current code. KV-cache per-step deltas remain unvalidated on real hardware: no KV timeline was scraped in that run.
- **Date**: 2026-07-17
- **Deciders**: Michele Campi

## Context

An agentic trajectory is a multi-step task: a driver (here, LangChain Deep
Agents) alternates LLM calls and tool executions until the task completes.
Every LLM call in the trajectory hits the same serving endpoint; the tool
steps run outside the engine but inside the task's wall-clock. The market
prices this unit — the *task* — in dollars per trajectory. Nobody prices it
in joules, and nobody reports how the engine's prefix cache behaves across
the steps of one task. inferscope already measures both quantities at
whole-run granularity (tokens-per-joule, ADR-010; cache hit-rate, ADR-011).
This ADR extends that measurement to the granularity the agentic market
actually transacts in: the step, and the trajectory as the sum of its steps.

### Position relative to ADR-012: the other side of the wall

ADR-012 established a wall: prefill and decode are *concurrent on the
device* — under continuous batching they interleave inside the same batch,
so no temporal cut isolates a phase, and any per-phase energy figure is an
apportionment, not a measurement. Trajectory steps sit on the other side of
that wall. The steps of one trajectory are *sequential by construction*: the
driver does not issue step N+1 until step N returns. Under controlled
concurrency — one trajectory in flight, no co-tenant load — the device-level
NVML energy counter delta over a step's time window is attributable to that
step by the same window-based semantics ADR-010 uses for the whole run. The
per-step figure is therefore a **measurement**, not an apportionment. That
contrast with ADR-012 is the claim of this ADR, and it holds exactly as far
as the concurrency condition holds (see the wall, below) and no further.

### The cache angle

Multi-step trajectories are the natural habitat of prefix-cache reuse: each
LLM call re-sends the system prompt plus the growing conversation history,
so the cacheable prefix grows monotonically within a task. A whole-run
hit-rate (ADR-011) averages over that curve; per-step hit-rate deltas expose
it — early steps cold, later steps increasingly served from cache. The
energy and cache stories are the same story: cache hits skip prefill
compute, and the per-step tok/J trajectory should reflect it. inferscope
already samples both series per tick on the shared clock (ADR-003); what is
missing is only the cut that maps ticks onto steps.

### What is missing, concretely

Two things, and only two. First, a source of step boundaries: inferscope
observes the engine, not the driver, and no engine-side counter demarcates
"step". Second, an absolute time anchor: the Report's timeline is relative
(`elapsed_ns` since an unrecorded reference instant, ADR-003), so a
driver-side timestamp cannot today be mapped onto it — `otel.rs` works
around this by inventing an anchor at export time (`SystemTime::now()` at
line ~116), a known defect. Both gaps are closed by this ADR's decision.

## Decision

### Driver-side step markers, offline join (inferscope stays a pure sampler)

inferscope does not learn about steps at runtime. The driver — the process
that already knows where steps begin and end, because it creates them —
emits a JSONL file of step boundaries, one object per step:

    {"step_id": 3, "kind": "llm_call", "t_start_unix_ns": ..., "t_end_unix_ns": ...}

with `kind` one of `llm_call` | `tool`, timestamps in UTC unix-epoch
nanoseconds read from the driver's wall clock at the boundary instants. The
schema is deliberately minimal and framework-agnostic: any agent framework
that can write four fields to a file can produce it, and Deep Agents is a
driver choice of the validation run, not a dependency of the format.

Correlation happens entirely offline, in `is-report`, after the run: the
profiler's timelines on one side, the driver's JSONL on the other, joined on
the clock. During the run the two processes never communicate — the same
passive-observer symmetry ADR-009 established for `--sample-only` (attach to
a live server without the server knowing) and ADR-003 established for
sysmon/GPU correlation (independent samplers, one shared reference, join
afterwards). inferscope keeps zero inbound API surface, remains read-only
with respect to the observed system, and no annotation traffic exists to
perturb the timing it is trying to measure.

Tool steps are first-class in the format even though no tokens flow during
them: their windows carry NVML energy (idle draw, or whatever the tool
itself puts on the device), and making tool-time energy visible — the cost
of the agent *not* talking to the model — is part of the per-task story the
dollar accounting also misses.

### Prerequisite: absolute anchor on the Report

The join needs the Report's relative timeline mapped onto the driver's
wall-clock timestamps. Today it cannot be: `elapsed_ns` counts from an
ADR-003 reference instant the Report never records. This ADR adds one
field:

    reference_instant_unix_ns: Option<u64>   // #[serde(default)]

captured via `SystemTime::now()` in the same statement sequence that takes
the monotonic reference instant, at run start. `Option` + `serde(default)`
keeps every pre-ADR-013 report deserialising unchanged — the same seam
discipline as `kvcache` (ADR-011) and `phase_energy` (ADR-012).

The rigor note, stated rather than assumed: pairing one wall-clock reading
(the anchor) with a monotonic clock (all deltas) is the standard
construction — wall-clock is read once, so NTP adjustments during the run
cannot bend the timeline; the anchoring error is a per-run constant on the
order of microseconds, negligible against steps measured in seconds. The
residual assumption, named in the report, is that driver and profiler read
clocks synchronised to within that same order — trivially true in the
validation setup, where both run on the same host.

The field also retires a known defect: `otel.rs` currently invents its
anchor at export time (`SystemTime::now()` when the span is built), which
places spans at export wall-clock rather than run wall-clock. With the
anchor recorded at run start, OTLP export uses it when present and falls
back to the current behaviour when absent.

### Join: step windows sliced from existing timelines

With the anchor in hand the join is arithmetic, not machinery. Each step's
window in Report coordinates is:

    step_start_elapsed_ns = t_start_unix_ns - reference_instant_unix_ns
    step_end_elapsed_ns   = t_end_unix_ns   - reference_instant_unix_ns

and every existing timeline is sliced by binary search over `elapsed_ns` —
the exact access pattern ADR-003 already names for sample lookup. The
sliced series are the ones the house already produces per tick: GPU power
samples (ADR-010), KV-cache counters (ADR-011), phase counters (ADR-012).
For the counter series, per-step figures come out as window deltas between
the samples bracketing the step boundaries. The whole-run semantics of
ADR-011 and ADR-012 — first sample to last sample — does not carry over
unchanged to an interior window, and saying it did was the flaw corrected
below: on the whole run the first sample *is* the start, but a step window
opens between two samples, and taking the first interior sample as baseline
discards whatever the counter did between the window start and that sample.
The baseline is therefore the last sample at or before the window start,
falling back to the first interior sample only when the window opens before
any sample exists. Nothing about the sampling side changes: same tick, same
samplers, same raw types. The step cut exists purely in `is-report`.

One consequence is worth naming: step boundaries fall between samples, so
each step window is resolved to the sampling grid, and a step shorter than
the sample period yields at most one bracketing interval. This is inherent
to a sampling profiler and is declared per-step (each `StepMetrics` carries
the number of samples its window contained) rather than hidden by
interpolation, which would manufacture precision the sampler does not have.

Energy is the one series with no per-tick cumulative counter to delta: the
NVML energy counter (ADR-010) is read as a whole-run delta, not sampled per
tick, and the sampling side does not change here. Per-step energy is
therefore the trapezoidal integral of sampled power over the inter-sample
segments fully contained in the step window — the same integration basis
ADR-010 already uses as its fallback — carried in integer arithmetic
(doubled mW·ns, floored to mJ once at the end). A segment straddling a step
boundary belongs to no step and lands in the unattributed remainder. Two
consequences follow. First, the reconciliation is exact by segment
accounting, not approximate: disjoint step windows partition the run's
segments into per-step subsets plus a remainder, so steps plus unattributed
equal the whole-run figure on the same basis, and the tests assert
equality. Second, the trajectory layer's `total_energy_mj` is
integration-based even when the ADR-010 headline figure is counter-grade;
the two are separate figures on declared bases, and mixing them would make
the reconciliation inexact by construction — the report keeps them apart
rather than reconciling across bases.

The baseline correction has a cost, stated rather than hidden: the baseline
sample may sit up to one sample period before the window opens, so a step
delta can include counter activity that preceded the step. At sample periods
comparable to step durations neither convention is exact. The choice is
between an error that is visible in the figures (a delta slightly too large,
bounded by one sample period of engine activity) and one that is not (a
systematic zero indistinguishable from a step that genuinely did nothing).
Over-attribution is the recoverable failure; it is preferred. Steps shorter
than the sample period still resolve no counter delta at all and report
absence — `samples_in_window` is carried per step so a reader can tell which
case they are looking at.

Absence and zero are distinct in the schema. The counter deltas on
`StepMetrics` are `Option`: `None` when the corresponding timeline was never
scraped, `Some(0)` when it was scraped and the counter did not move. Writing
an unobserved counter as `0` makes an absent measurement indistinguishable
from a measured idle step for any consumer of the JSON, which is the same
failure mode ADR-0007 phase A removed from the operator's placement signals
and the same discipline `tokens_per_joule` and `cache_hit_rate` already
followed here.

### Derived layer: `TrajectoryMetrics` in `is-report`

Two derived types, modelled on `KvCacheMetrics` and `PhaseEnergyMetrics`:

    pub struct StepMetrics {
        pub step_id: u64,
        pub kind: StepKind,              // LlmCall | Tool
        pub start_elapsed_ns: u64,
        pub end_elapsed_ns: u64,
        pub samples_in_window: u64,      // grid resolution, declared per step
        pub energy_mj: u64,              // NVML delta over the step window
        pub generation_tokens_delta: u64,
        pub prompt_tokens_delta: u64,
        pub cache_hits_delta: u64,
        pub cache_queries_delta: u64,
        // derived floats, at the edge as always
        pub tokens_per_joule: Option<f64>,   // None for tool steps / zero tokens
        pub cache_hit_rate: Option<f64>,     // None when queries_delta == 0
    }

    pub struct TrajectoryMetrics {
        pub steps: Vec<StepMetrics>,
        pub total_energy_mj: u64,
        pub total_generation_tokens: u64,
        pub trajectory_tokens_per_joule: Option<f64>,
        pub llm_energy_mj: u64,          // sum over LlmCall steps
        pub tool_energy_mj: u64,         // sum over Tool steps
        pub unattributed_energy_mj: u64, // run energy not inside any step window
    }

The integer/float discipline holds: raw deltas are `u64`, ratios are the
only floats and live here, in the report layer. `unattributed_energy_mj` is
load-bearing rather than cosmetic: steps need not tile the run (driver
startup, gaps between steps, teardown), and the energy outside every step
window is reported as its own figure instead of silently vanishing — the
per-step figures plus the unattributed remainder reconcile to the ADR-010
whole-run energy, and that reconciliation is asserted in tests.

Withholding rules, in the ADR-011/012 discipline: `TrajectoryMetrics` is
`None` when the Report lacks `reference_instant_unix_ns`, when no step file
is supplied, or when any counter regresses within the run. A single step is
dropped (and named in a `dropped_steps` diagnostic list) when its window
falls outside the run window or overlaps a neighbour — overlapping steps
would double-count energy, and double-counting is the one error this design
must never commit. Tool steps legitimately carry zero token deltas; their
`tokens_per_joule` is `None`, not `0.0`, because "no tokens" is absence,
not a measured zero efficiency.

### The wall: controlled concurrency only

The per-step energy figure is a measurement under one condition: the step's
time window contains only that step's work. One trajectory in flight, no
co-tenant load, tool steps that do not overlap LLM calls — then the NVML
delta over the window is the device cost of the step plus idle draw, and
the report can say "measured". Break the condition — two trajectories
interleaved under continuous batching, a co-located workload on the same
device — and the window mixes contributions with no temporal cut to
separate them: the figure collapses back into exactly the ADR-012 wall, an
apportionment problem this ADR does not solve and does not claim to.

The condition is therefore declared, not assumed: validity holds at
controlled concurrency only, the report states it next to the numbers
(012-style), and the validation run enforces it by construction (one
driver, one trajectory at a time, dedicated device). Extending attribution
to concurrent trajectories would require per-request engine-side signals
and an apportionment model — named as future work in Alternatives, not
smuggled in here.

## Consequences

### Positive

- inferscope prices the unit the agentic market transacts in — the task —
  in joules and cache behaviour, per step, on hardware already in use.
  Nobody publishes this axis; the dollar accounting (NemoClaw-style
  blueprints included) has no energy or cache column.
- Per-step figures are measurements under a declared condition, not
  apportionments — a strictly stronger claim than ADR-012's, obtained by
  choosing a granularity (sequential steps) where the temporal cut exists.
- Reuse is near-total: same tick, same samplers, same raw types, same
  binary-search access pattern. The entire feature lives in one new Report
  field (the anchor), one input file format, and one derived module in
  `is-report`. The anchor also retires the `otel.rs` export-time-anchor
  defect.
- The reconciliation invariant (steps + unattributed = whole-run energy)
  makes the numbers auditable arithmetic, not a dashboard.

### Negative

- Validity is conditional on controlled concurrency, so the headline
  figures do not describe production serving of concurrent agents; the
  report says so, and the claim is calibrated accordingly.
- The step file is trusted input: inferscope cannot verify that the
  driver's timestamps are honest or its steps truly sequential. Overlap
  detection catches malformed files, not wrong clocks. Same-host operation
  is assumed for clock agreement and named in the report.
- A step shorter than the sample period resolves to at most one bracketing
  interval; fine-grained sub-second steps need a faster tick, with the
  sampling-overhead trade that implies.
- One more optional Report field and one more derived module enlarge the
  surface — proportionate, as in ADR-012, to a positioning-relevant metric.

## Validation boundary (VM vs GPU)

- **VM milestone (this work)**: the join and derivation logic — anchor
  field, JSONL ingestion, window slicing, `StepMetrics`/`TrajectoryMetrics`,
  every withholding and dropping branch, the reconciliation invariant —
  built and green against fixtures: a synthetic step JSONL plus the existing
  metrics fixture and llm-d CPU sim. No physical joule figure is claimed;
  the energy series is whatever the fixtured path supplies, and tests
  assert the logic.
- **Real-energy validation (GPU pass, after 2026-08-11)**: LangChain Deep
  Agents driving multi-step tasks against a local vLLM endpoint, the same
  model size already in use in prior experiments (not Nemotron-class), one
  trajectory at a time on a dedicated device. Expected read-outs: the
  per-step cache-reuse curve (cold early steps, warm late steps), the
  per-step tok/J trajectory tracking it, and the tool-time/unattributed
  energy share. The article hook is the market gap: agentic cost is priced
  per task in dollars; this measures the same unit in joules and cache.

## Alternatives Considered

### Annotation API in inferscope (driver calls the profiler)

Rejected. An inbound endpoint couples the profiler to whichever framework
integrates the client, adds a write path to a tool that is read-only toward
the observed system, and delivers boundary marks through a network hop with
its own latency — perturbing exactly the timing being measured. The offline
join keeps the ADR-009 passive-observer symmetry and needs nothing from the
framework but four fields in a file.

### Inferring step boundaries from the metrics themselves (no markers)

Rejected. Token-counter activity gaps could segment LLM calls heuristically,
but the inference is ambiguous at the sampling grid (short gaps vanish
between ticks), cannot distinguish tool steps from idle, and turns a
measurement design into a detection problem with error bars of its own. The
driver knows the boundaries exactly and for free; guessing at what is
already known is rigor theatre.

### Per-request attribution via engine-side request IDs

Deferred, not rejected. Engine-side per-request signals (as scheduler-level
work in the llm-d ecosystem moves toward) could tie tokens to requests
under real concurrency — but energy would still be device-level, so
concurrent attribution remains an apportionment problem (ADR-012's wall) no
matter how good the request-level token accounting gets. Named as the
future-work path for lifting the concurrency condition; out of scope here.
