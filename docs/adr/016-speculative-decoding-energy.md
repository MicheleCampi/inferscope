# ADR-016: Speculative Decoding Energy Attribution

- **Status**: Accepted
- **Validation**: Stage one complete, 2026-09-05, 1x H100 PCIe. Eleven runs,
  session valid (baseline drift 0.13% against a 5% tolerance; realized
  acceptance length matched the configured value on all nine points). The
  crossover this ADR set out to find does not exist in L in [1, 6]:
  speculation is cheaper per committed token at every point, including at
  zero acceptance, where it still costs 0.897x the baseline. Full figures and
  the source-level checks behind them in
  `validation-results/adr-016-h100-spec/RESULTS.md`.
- **Validation as of 2026-09-03, when this ADR was written**: none. The raw
  layer (`SpecSample`, `SpecTimeline`) was built and green against unit tests;
  the parser, the scrape loop and the orchestrator seam were the work this ADR
  scoped. No speculative run had been measured on hardware. The vLLM-side
  facts this ADR rests on were verified at source against
  `vllm-project/vllm` at `27a94d1` (2026-09-02), not inferred from
  documentation. Kept as written: the decisions below were taken against that
  state of knowledge, and reading them without it would make them look better
  founded than they were.
- **Date**: 2026-09-03
- **Deciders**: Michele Campi

## Context

Speculative decoding is tuned on latency. A draft model proposes k tokens, the
target model verifies them in one forward pass, and the accepted prefix is
committed. The figures the technique is reported on - acceptance rate, mean
acceptance length, tokens per second - are all throughput figures.

None of them says what the rejected drafts cost. A draft token that fails
verification consumed a forward pass on the draft model and a verification slot
on the target model, and produced nothing. That work is real, it is paid in
joules, and it is invisible to every latency benchmark: a run with 30%
acceptance and a run with 90% acceptance can post similar tokens-per-second
while drawing materially different energy per committed token.

inferscope already holds both halves. ADR-010 reads device energy via NVML;
ADR-011 scrapes the engine's Prometheus endpoint; ADR-003 puts both on one
reference instant. What was missing is the speculative counter family, and a
way to sweep acceptance as an independent variable rather than observing
whatever a given draft model happens to produce.

### What vLLM exposes (verified at source)

`vllm/v1/spec_decode/metrics.py`, class `SpecDecodingProm`, registers three
`prometheus_client.Counter` families:

- `vllm:spec_decode_num_drafts` - speculation rounds
- `vllm:spec_decode_num_draft_tokens` - tokens proposed
- `vllm:spec_decode_num_accepted_tokens` - tokens that survived verification

All three are exposed with the `_total` suffix the library appends, the same
rule that made the ADR-011 prefix-cache series unreadable until it was found.
A fourth family, `vllm:spec_decode_num_accepted_tokens_per_pos`, carries the
acceptance profile split across a `position` label. See D5.

`SpecDecodingProm.__init__` returns before registering anything when
`speculative_config is None`. This is load-bearing and is treated in D1.

SGLang exposes speculative decoding as gauges over the current batch
(`sglang:spec_accept_length`, `sglang:spec_accept_rate`) plus configuration
readouts (`sglang:spec_num_steps`, `sglang:spec_num_draft_tokens`). A gauge
carrying a current mean has no meaning under the window differencing this
crate performs - the same reason `sglang:cache_hit_rate` is not read
(ADR-014). The cumulative counts have no SGLang counterpart, so this is a
capability gap.

### The independent variable

`vllm/config/speculative.py` exposes `rejection_sample_method='synthetic'`,
which accepts draft tokens against a declared per-position rate vector instead
of against the target model's distribution. It is configurable two ways, and
they are mutually exclusive:

- `synthetic_acceptance_rates`: an explicit vector of *unconditional*
  per-position rates, length `num_speculative_tokens`, each in `[0, 1]`,
  monotonically non-increasing. Entry i is the marginal probability that the
  first i+1 draft tokens are all accepted.
- `synthetic_acceptance_length`: a scalar target mean acceptance length in
  `[1, num_speculative_tokens + 1]`, resolved internally to the vector above.

The mechanism lives in the shared rejection sampler, not in any one proposer,
so it composes with every speculation method including ngram. This turns
acceptance from something the run suffers into something the run declares,
which is what makes an energy crossover findable rather than merely observable.

Critically, synthetic mode substitutes only the accept/reject *decision*. The
draft proposal and the target verification forward pass both execute
unchanged, so the energy measured under synthetic acceptance is the energy of
real speculative work. Were this not so, the whole campaign would be measuring
an artefact. Verified in `rejection_greedy_sample_kernel`: the `SYNTHETIC_MODE`
branch replaces the comparison against `target_argmax_id`, nothing upstream.

## Decision

### D1: Two absences collapse, and neither is a defective body

`parse_phase` treats a series the schema declares but the body does not carry
as a `Parse` error, on the reasoning that the schema said it would be there
(ADR-014 D3). For the speculative family that reasoning does not hold, because
of the early return in `SpecDecodingProm.__init__`: a vLLM server started
without a speculative config registers no counters at all. Such a body is not
defective - it is a server that is not speculating.

So two distinct absences map onto the same outcome, `Ok(None)`:

1. the engine has no such capability (SGLang), and
2. the engine has the capability but this run did not enable it (vLLM without
   a speculative config).

The parser does not distinguish them and should not: from the reader's side
they are the same fact, which is that this run produced no speculative
measurement. Neither is a zero. This is a deliberate divergence from
`parse_phase`, and it is pinned by a test asserting that the existing
non-speculative vLLM fixture yields `(None, None, None)` rather than an error.

An `Err` remains reserved for what it always meant: a line that exists and
does not parse.

### D2: A third scrape loop, not a third read of one body

The speculative scrape is its own `tokio` task with its own GET, separate from
the KV loop (ADR-011) and the phase loop (ADR-012), for the reason ADR-012
already established: these are independent first/last reductions over the same
run window. They share `start` and the cancel discipline - the shared clock the
correlation with energy rests on - and nothing else. Folding them into one GET
would couple three derivations whose failure modes are unrelated, and would put
the green ADR-011 path at risk for a saving of two HTTP round-trips against a
localhost endpoint.

Same contract as the other two loops, without exception:
`MissedTickBehavior::Skip`, a `biased` cancel arm that wins over the tick,
per-tick errors swallowed, and an empty timeline rather than a propagated error
when the client cannot be built.

### D3: The scrape returns a reading, not a sample

`scrape_once` converts an absent hit-rate numerator into `Err`, because
`KvCacheSample` has no way to express absence. `SpecSample` has the same
limitation, but the decision was already placed elsewhere:
`SpecTimeline::push` takes three `Option<u64>` and drops the tick if any is
missing, because the three counters are one capability and a subset is not a
measurement.

So `scrape_spec_once` returns `(u64, Option<u64>, Option<u64>, Option<u64>)` -
the elapsed timestamp and the three readings - and the loop hands them straight
to `push`. Returning `Result<SpecSample, _>` would force the I/O layer to make
a decision that is already made in `is-core`, and would leave `push`'s
partial-family branch unreachable from the only path that calls it.

`push`'s boolean return is discarded in the loop, for the same reason per-tick
errors are: a dropped tick is a normal outcome of best-effort sampling.

### D4: Mean acceptance length includes the bonus token

vLLM computes `mean_acceptance_length = 1 + (num_accepted_tokens /
num_drafts)`, and `synthetic_acceptance_length` is expressed in the same
convention - which is why its domain is `[1, k+1]` and not `[0, k]`: the
internal resolution computes `num_drafts = length - 1` before distributing
across positions.

Any derived acceptance length in inferscope carries the `+1`. Dropping it would
make every measured length exactly one lower than the knob that produced it,
which on an energy sweep does not look like an error - it looks like a
crossover in the wrong place. The convention is recorded on
`SpecSample::drafts` at the point of use, not only here.

### D5: The per-position family is not read, for now

`vllm:spec_decode_num_accepted_tokens_per_pos` carries the acceptance profile
directly, which is the shape the campaign manipulates. It is deferred for two
reasons.

First, cost: it is a labelled vector, not a scalar. Reading it requires an
`Aggregation` variant that returns something other than `u64`, which changes
the `Series` type rather than extending it - a wider change than the whole rest
of this ADR.

Second, value: under synthetic acceptance the profile is *declared*, not
discovered. Reading it back verifies that the engine did what it was told,
which the three scalars already do via the realized mean length. It becomes
worth its cost only if the two-arm comparison in D7 shows the variance
structure matters, at which point it stops being a verification and becomes a
measurement.

Until then it is guarded rather than ignored: the fixture carries per-position
lines, and a test pins that the exact-name match excludes them from the
accepted-tokens reading. This is the same trap `_created` posed, and the same
defence.

### D6: Wired into both run paths, and `--sample-only` is the campaign path

The scrape is spawned in `orchestrate` and in `run_sample_only`. The second is
not an afterthought: the campaign drives load from an external generator
against a vLLM server started with a speculative config, and attaches
inferscope to the server PID. That is `--sample-only` by construction. An
implementation wired only into `orchestrate` would collect nothing on the exact
run the ADR exists to enable.

`ResourceReport` therefore gains `spec_timeline: Option<SpecTimeline>` beside
`phase_timeline`, with serde `default` and `skip_serializing_if` so archived
reports deserialize unchanged. `Report` gains the same field.

`orchestrate` now carries three scrape tasks and three cancels through one
binding. That tuple stops being readable at three, and becomes a named struct.

### D7: What this does not claim

The energy figure is whatever ADR-010 measured for the device over the window,
and every limit ADR-012 named about device-level energy holds here unchanged.
On top of those, three limits are specific to this ADR:

1. **Synthetic acceptance is not model acceptance.** A sweep over
   `synthetic_acceptance_length` measures how energy responds to acceptance
   holding the model fixed. It does not tell you what acceptance a given draft
   model achieves. Those are different questions and the report says which one
   it answered.

2. **The internal resolution is minimum-variance.**
   `_acceptance_length_to_rates` distributes a target length as
   `[1.0] * floor(L-1) + [frac] + [0.0] * rest` - the first positions always
   accepted, one fractional, the rest never. The mean is exact; the variance is
   the lowest achievable for that mean. Real speculative acceptance decays
   roughly geometrically and is far more dispersed, and dispersion changes the
   per-step committed-token distribution, which is what the batch shape and
   therefore the energy per step depend on. A sweep via the scalar knob
   measures the minimum-variance envelope, not the realistic curve.

   The campaign therefore runs two arms at matched mean length: the scalar
   knob, and an explicit geometric vector via `synthetic_acceptance_rates`
   (`p^(i+1)`, with p solved so that `sum(rates) = L - 1`). The falsification
   criterion is declared before the runs: if the crossover point does not move
   between arms, the scalar knob is validated as a sufficient instrument and
   the geometric arm is dropped from later campaigns. If it moves, the scalar
   knob alone is reported as insufficient, and that is the finding.

3. **The configured length is an upper bound, not the realized one.** The
   verification kernel rejects padded draft slots (`draft_token_id >= 0`), and
   ngram proposal frequently returns fewer than k tokens. A run configured at
   length L can realize less. This is precisely why the three counters are
   scraped rather than assumed: `1 + accepted/drafts` over the window is the
   measurement, and the configured value is only the setting. Any report that
   states an acceptance length states the measured one.

## Consequences

### Positive

- Puts the energy cost of rejected speculation on the same clock as the work
  that produced it, with acceptance as a declared independent variable rather
  than a property of whichever draft model was to hand.
- Additive throughout: one parser function, one scrape loop pair, one optional
  field on each report type. `parse_series` is unchanged, and the ADR-011 and
  ADR-012 paths are untouched.
- The absence semantics in D1 mean a non-speculative run degrades to an empty
  section rather than a failed scrape, so the flag is safe to leave on.

### Negative

- A third loop means a third GET per tick against the same endpoint. Judged
  acceptable for a localhost scrape at the cadences in use, and the alternative
  couples three independent derivations.
- The per-position family is left on the table (D5), so the acceptance profile
  is declared-and-verified rather than measured.
- The `--sample-only` KV gap is not closed here. It predates this ADR and is
  widened by it in the sense that the campaign path now carries two of the
  three scrapes and not the third. Recorded, not fixed.

## Alternatives Considered

### Deriving acceptance from a single counter pair

Rejected. Acceptance rate needs draft and accepted; mean acceptance length
needs the round count as well, and the round count is what the engine's own
tuning knobs are expressed in (D4). Two of three is not a smaller version of
the measurement, it is a different one.

### Reading SGLang's speculative gauges

Rejected, on the ADR-014 reasoning. `sglang:spec_accept_length` is a mean over
the current batch. Differencing a mean over a window yields nothing, and
sampling it at a cadence yields a series whose relationship to the window total
depends on batch composition at each tick. The honest reading is `None`.

### Observing acceptance from a real draft model instead of imposing it

Rejected as the primary design, kept as future validation. A real draft model
produces one acceptance profile per (draft, target, workload) triple, which
makes acceptance a confound rather than a variable: two points on such a sweep
differ in the draft model as well as in acceptance, and the energy difference
cannot be attributed to either. The synthetic path holds everything else fixed.
Validating a synthetic point against a real draft model at the same measured
acceptance length is the natural follow-on, and is out of scope here.
