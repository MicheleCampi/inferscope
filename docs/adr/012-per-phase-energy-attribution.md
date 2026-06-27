# ADR-012: Per-Phase Energy Attribution (Prefill vs Decode)

- **Status**: Proposed
- **Date**: 2026-06-27
- **Deciders**: Michele Campi

## Context

ADR-010 turned instantaneous power into energy and derived the
efficiency family — `energy_joules`, `energy_per_token_mj`,
`tokens_per_joule`, `tokens_per_watt`. Those figures are *whole-workload*:
one energy delta over the probe window, one token count, one ratio. They
answer "what did this run cost per token" but not "what did each phase of
generation cost per token." Prefill (prompt ingestion, compute-bound) and
decode (autoregressive generation, memory-bandwidth-bound) have different
hardware cost structures; a single tokens-per-joule averages over both and
hides the split the energy literature now asks for — phase-aligned
attribution that maps energy onto the transformer's internal phases rather
than reporting one workload-level number.

inferscope is unusually well placed to attempt this *software-only*. The
field's per-phase energy work depends on external hardware metering. ADR-010
already reads device energy via NVML; ADR-011 already scrapes the engine's
Prometheus endpoint on the shared `elapsed_ns` clock. The phase signals
needed are, on inspection of the authoritative fixture
(`crates/is-metrics/tests/fixtures/llm-d-inference-sim-v0.8.2-metrics.txt`),
already present in that scrape. The question this ADR settles is not "can we
fetch phase signals" — we can — but "what can NVML device-level energy
honestly be attributed to a phase, and where is the limit that no
software-only method crosses."

That honest boundary is the deliverable. A documented "this much is
attributable, and here is the wall" is a publishable result, not a failure.

### What the scrape actually exposes (verified at source, not assumed)

Grepping the fixture for phase-bearing `vllm:` series, then reading their
values, gives three categories:

- **Phase token counters** — `vllm:prompt_tokens_total` (196 on the fixture)
  and `vllm:generation_tokens_total` (38). Pure monotonic counters, integer
  values, identical in shape to the `prefix_cache_*` series ADR-011 already
  parses. No parser extension needed for these.
- **Phase time histograms** — `vllm:request_prefill_time_seconds_*` and
  `vllm:request_decode_time_seconds_*`. The `_sum` line of each is a single
  numeric line of the same `metric{labels} value` form as a counter
  (`prefill_time_sum = 1.4493e-5`, `decode_time_sum = 2.8432e-5` on the
  fixture); only the `_bucket{le=...}` lines carry the per-bucket cumulative
  shape ADR-011 called dead weight. ADR-012 needs the `_sum`, not the
  buckets — so ADR-011's YAGNI on histogram-bucket parsing stands untouched.
- **Discarded** — `vllm:time_to_first_token_seconds_sum` is `0` on the sim:
  the simulator does not model TTFT. It is therefore not a reliable phase
  signal here and is not used.

### The consistency check that defines the limit

On the fixture, summing the phase times does not reconstruct the engine's
own inference time, let alone wall-clock:

    prefill_time_sum + decode_time_sum = 1.4493e-5 + 2.8432e-5 = 4.29e-5 s
    request_inference_time_seconds_sum                          = 1.34e-3 s

The phase times account for ~3% of inference time. This is not a batching
artefact (the CPU sim runs one request at a time); inference time includes
queueing, scheduling, and per-iteration overhead that belongs to neither
phase. On a real GPU under continuous batching / chunked prefill the gap is
compounded by genuine phase *interleaving*: prefill of one request and
decode of another execute in the same batch on the same device, so the NVML
device-level energy counter — cumulative, one number for the whole device —
has no clean temporal cut that isolates a phase. There is no instant at which
the device is "doing prefill" and not decode.

This is the wall. Any per-phase energy figure is therefore an
*apportionment*, not a measurement: a model that projects the measured
device energy onto a phase using a phase-share ratio, not a reading of the
energy a phase physically spent.

## Decision

### Two apportionments, and their divergence as the first-class signal

inferscope does not pick one apportionment and reject the other. It exposes
**both**, and treats their disagreement as the metric. The reasoning is the
crux of this ADR: a single apportionment can only ever restate its own
premise, but the *gap* between two apportionments built on different premises
measures the thing per-phase energy exists to expose.

**Time-share** apportions device energy in proportion to time spent in each
phase, from the phase-time `_sum` deltas:

    share_prefill_time = prefill_ns_delta / (prefill_ns_delta + decode_ns_delta)
    energy_prefill_by_time_mj = energy_millijoules * share_prefill_time
    energy_decode_by_time_mj  = energy_millijoules * (1 - share_prefill_time)

**Token-share** apportions the same energy in proportion to token counts:

    share_prefill_tok = prompt_tokens_delta / (prompt_tokens_delta + generation_tokens_delta)
    energy_prefill_by_tokens_mj = energy_millijoules * share_prefill_tok
    energy_decode_by_tokens_mj  = energy_millijoules * (1 - share_prefill_tok)

Both are projections of one measured device-energy delta onto a phase basis;
neither measures the joules a phase physically drew (see the wall, below).
But their premises differ, and that is the point:

- time-share assumes **energy tracks time-in-phase** at roughly constant mean
  power;
- token-share assumes **energy tracks token count** — equal energy per token
  across phases.

The second premise is false in a known direction: prefill is compute-bound,
decode is memory-bandwidth-bound, so per-token energy is *not* equal across
phases. That falsity is exactly why token-share is kept rather than discarded.
The **divergence** between the two apportionments —

    phase_energy_divergence = share_prefill_time - share_prefill_tok

— quantifies the per-token energy asymmetry between the phases. If it is near
zero, the phases cost roughly the same energy per token and the split carries
little information. The further it departs from zero, the more the phases
differ in per-token energy cost — the compute-bound/memory-bound asymmetry,
read out as a single number on hardware inferscope already samples. The report
carries both apportionments and the divergence, and states plainly that the
divergence, not either apportionment alone, is the load-bearing figure.

This mirrors ADR-010's treatment of `tokens_per_watt` and
`tokens_per_joule`: two framings of related quantities, exposed together with
the relationship between them stated, rather than one chosen and the other
suppressed. Here the relationship is not an identity (as it was there) but a
*difference*, and the difference is the signal.

### Raw layer: a `PhaseSample` timeline, integer-only, on the shared clock

A new raw type mirrors `KvCacheSample` (ADR-011) and the integer discipline
ADR-005 established and ADR-010 kept (`GpuSample` power is `u64` milliwatts,
`DeviceEnergy` is `u64` millijoules — the house keeps no `f64` in the raw
layer, so every raw sample derives `Copy + Eq`):

    pub struct PhaseSample {
        pub elapsed_ns: u64,          // shared reference instant (ADR-003)
        pub prompt_tokens: u64,       // vllm:prompt_tokens_total
        pub generation_tokens: u64,   // vllm:generation_tokens_total
        pub prefill_ns: u64,          // request_prefill_time_seconds_sum, *1e9
        pub decode_ns: u64,           // request_decode_time_seconds_sum,  *1e9
    }

The two phase times arrive as float seconds on the wire. They are converted
to integer nanoseconds **at parse time** — the same unit `elapsed_ns` already
uses — so the raw layer stays integer-only and `Eq`, and the float lives only
in the derived ratios in `is-report`. This preserves the `Copy + Eq`
invariant every other raw sample holds and keeps the float-at-the-edge
discipline whole.

`PhaseTimeline { samples: Vec<PhaseSample>, sample_period_ns: u64 }` mirrors
`KvCacheTimeline` exactly: per-tick samples (the phase split evolves over a
run — early ticks are prefill-dominated, later ticks decode-dominated, and
that curve is signal), `push`, `len`, `is_empty`. Window deltas are taken
from first and last sample, as ADR-010/011 do for monotonic series.

### Parser: one float-reading sibling, additive

`is-metrics/parse.rs` already has a generic `parse_counter(body, metric,
model_name) -> u64`. The phase **token** counters reuse it unchanged (new
call-sites, no engine change). The phase **time** `_sum` values cannot:
`parse_counter` does `value as u64`, which truncates `1.4493e-5` to `0`. A
sibling `parse_seconds_as_nanos(body, metric, model_name) -> u64` reads the
same `metric{labels} value` line, parses the value as `f64` (the existing
code already reads values as `f64` before converting — only the conversion
differs), and returns `(seconds * 1e9).round() as u64`. This is the minimal
extension: it does **not** touch histogram-bucket parsing, so ADR-011's
boundary holds.

### Derived layer: `PhaseEnergyMetrics` in `is-report`

Modelled on `DeviceEnergy` (window value + provenance) and `KvCacheMetrics`
(window deltas + derived ratio). It carries both apportionments and the
divergence:

    pub struct PhaseEnergyMetrics {
        pub prefill_ns_delta: u64,
        pub decode_ns_delta: u64,
        pub prompt_tokens_delta: u64,
        pub generation_tokens_delta: u64,
        // time-share apportionment
        pub energy_prefill_by_time_mj: u64,
        pub energy_decode_by_time_mj: u64,
        // token-share apportionment
        pub energy_prefill_by_tokens_mj: u64,
        pub energy_decode_by_tokens_mj: u64,
        // the first-class signal: time-share minus token-share (prefill side)
        pub phase_energy_divergence: f64,
    }

The integer energy figures stay `u64` millijoules (the `DeviceEnergy` unit);
the divergence is the one derived float, living in the report layer exactly
as `EfficiencyMetrics`' ratios and `KvCacheMetrics`' `hit_rate` do.

Validity conditions, made explicit rather than assumed (the ADR-011
discipline): if either phase-time delta is zero the time-share is undefined;
if either token delta is zero the token-share is undefined; in either case
the affected apportionment, and the divergence that depends on it, are
withheld (the struct is `None`) rather than reported as a divide-by-zero or a
meaningless figure. If any counter regresses within the window (engine reset)
the struct is withheld entirely, exactly as `KvCacheMetrics` is. The struct
is `None` when no `/metrics` endpoint is configured, when the phase series are
absent, or when no energy figure exists to apportion.

### Report seam

One optional field on `Report`, `#[serde(default, skip_serializing_if =
"Option::is_none")]` so pre-ADR-012 reports deserialise unchanged:

    phase_energy: Option<PhaseEnergyMetrics>

plus the raw `phase_timeline: Option<is_core::PhaseTimeline>`, folded in by
the orchestrator at the same stage `efficiency` and `kvcache` are folded.

### What inferscope does not claim (the wall, in the report)

The report states, next to the numbers, that per-phase energy is an
**apportionment of device-level energy, not a measurement of phase energy** —
and that this holds for *both* apportionments equally. Three limits are named:

1. NVML energy is device-level and cumulative; under interleaved execution
   (continuous batching, chunked prefill) no temporal cut isolates a phase
   on the device.
2. Phase times do not partition the window: `prefill + decode` is a fraction
   of inference time, which is itself a fraction of wall-clock. The
   time-share apportionment normalises over phase time, a basis that does not
   cover the energy window.
3. Both apportionments are projections, not causal measurements: each reports
   a phase-share (of time, or of tokens) and projects energy onto it; neither
   measures the joules a phase physically drew. The divergence between them
   is informative precisely because both are projections of the *same*
   measured energy onto *different* bases — it isolates the basis disagreement,
   which carries the per-token asymmetry, without either projection having to
   be true on its own.

## Consequences

### Positive

- inferscope expresses the prefill/decode energy split the literature asks
  for, software-only, on hardware already in use — where the field depends
  on external metering — and adds a divergence signal that no single
  apportionment carries.
- Reuses the whole ADR-003 clock / ADR-005 integer-raw / ADR-010 energy /
  ADR-011 scrape stack; the only new parsing is one float-to-nanos sibling,
  and ADR-011's histogram-bucket boundary is untouched.
- The stated wall makes the numbers safe to cite: two apportionment models
  with their premises and limits on the label, and a divergence whose meaning
  is stated, never an over-claimed measurement.

### Negative

- The per-phase figures are model output, not measurement; exposing two
  apportionments doubles the over-read surface. Mitigated by labelling both
  as models, stating that the divergence (not either alone) is the signal,
  and putting the shared wall next to them.
- A new raw type, a new derived type, and one parser function enlarge the
  surface. Judged proportionate to a frontier, positioning-relevant metric.
- TTFT, a natural phase-boundary signal, is unusable on the current sim
  (emits 0); the model rests on the phase-time sums alone until validated on
  an engine that populates TTFT.

## Validation boundary (VM vs GPU)

- **VM milestone (this work)**: the correlation/attribution *logic* — parser
  sibling, `PhaseSample`/`PhaseTimeline`, `PhaseEnergyMetrics` with both
  apportionments and the divergence, the report seam — built and green
  against the fixture and the llm-d CPU sim. The phase signals on the fixture
  are populated and consistent enough to test every branch (both
  apportionments, the divergence, validity withholding, regression
  withholding). No real per-phase energy number is produced on the VM; the
  energy term is whatever the fixtured/sim path supplies, and the test asserts
  the *logic*, not a physical joule figure.
- **Real-energy validation (separate GPU pass)**: folded into the CUDA-graphs
  energy re-run already scheduled on H100 — no extra GPU spend. There both
  apportionments are exercised against real NVML energy, and the divergence is
  stress-tested by **workload isolation**: a prefill-only and a decode-heavy
  regime (the disaggregation experiment already showed the spatial asymmetry —
  prefiller 127 W / 1% util vs decoder 466 W — that a phase split should
  reflect). If the divergence under isolation disagrees with what the spatial
  asymmetry implies, that disagreement is itself the publishable finding, and
  the wall section is where it is recorded.

## Alternatives Considered

### A single apportionment (time-share or token-share alone)

Rejected. Either one alone can only restate its own premise — time-share that
energy tracks time, token-share that energy tracks tokens. Neither is a
measurement, so picking one and suppressing the other discards the only thing
that carries information without hardware metering: the *gap* between them,
which keys on the per-token energy asymmetry between compute-bound prefill and
memory-bandwidth-bound decode. Both are exposed; the divergence is the signal.

### Parsing the phase-time histogram buckets

Rejected as unnecessary. The `_sum` line alone gives total time-in-phase,
which is all the time-share apportionment needs. Bucket parsing would reopen
the histogram-parser work ADR-011 scoped out, for no gain to this objective.

### Per-request phase energy

Out of scope, as in ADR-010 and ADR-011: inferscope profiles a server over a
window, not a single request. The per-phase figure is per-window, and the
report says so.

### Deferring until real per-phase energy can be measured directly

Rejected. Direct per-phase energy measurement needs hardware metering
inferscope explicitly does not assume. The honest software-only result — two
labelled apportionment models with a divergence signal and a documented wall —
is the deliverable, and it is fully buildable now.
