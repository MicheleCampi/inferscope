# ADR-016 Campaign Protocol — Speculative Decoding Energy Crossover

- **Status**: Declared, not yet run
- **Date**: 2026-09-03
- **Hardware**: 1x H100, single GPU
- **Target**: `meta-llama/Llama-3.1-8B-Instruct`
- **Draft**: `meta-llama/Llama-3.2-1B-Instruct`, `num_speculative_tokens=5`

Everything below is fixed before the first run. Criteria written after seeing
data are selection, not criteria.

## Question

At what mean acceptance length does speculative decoding stop costing more
energy per committed token than non-speculative decoding, and does that
crossover point depend on the dispersion of acceptance or only on its mean?

## Arms

Eleven runs: one baseline at the start, nine speculative configs, one baseline
repeated at the end.

- **baseline** — no `--speculative-config`. The crossover is defined against
  this; without it the sweep points only compare to each other.
- **min-variance** — L in {1, 2, 3, 4, 5}, the schedule vLLM resolves from a
  scalar `synthetic_acceptance_length`, passed here as an explicit vector.
- **geometric** — L in {2, 3, 4, 5}, `p^(i+1)` with p solved to match the
  mean. L=1 is omitted: both arms are the all-zero vector there.

Both arms pass `synthetic_acceptance_rates` explicitly so they travel the same
code path, and the comparison isolates dispersion.

## Load

Identical across every run. Any variation here lands in the energy figure and
cannot be separated from the knob afterwards.

- Fixed prompt set, fixed count, replayed in the same order every run.
- `temperature=0`. With sampling, output length varies between runs and the
  total work differs between points that should differ only in acceptance.
- Fixed `max_tokens`, fixed concurrency, fixed request rate.
- The load generator is external; inferscope attaches with `--sample-only`
  and `--gpu` (ADR-016 D6).

## Run order

Randomized, with the seed recorded in `run-order.txt` before the first run.

Thermal drift is the reason: the GPU is hotter at run ten than at run one, and
power at equal work rises with it. Under an order monotonic in L, that drift
adds to the independent variable and moves the apparent crossover. Randomizing
does not remove drift; it stops drift from correlating with L.

Between runs, a fixed cooldown, and the server is restarted for each config
(the speculative config is not hot-swappable).

## Per-run measurement

- Duration: 180s of steady-state sampling, after a 60s warm-up that is not
  sampled. Below roughly two minutes the NVML energy delta is too small
  relative to counter resolution.
- `--metrics-period` at the campaign default; every run emits `--json`.
- Realized acceptance length is read from the scraped counters as
  `1 + accepted/drafts` over the window (ADR-016 D4). The configured value is
  the setting; this is the measurement, and the report states the measured
  one.

## Discard criteria — declared in advance

A run or a session is discarded, not adjusted, when:

1. **The two baselines disagree.** If opening and closing baseline energy per
   token differ by more than 5%, the session is thermally or otherwise
   unstable and every run in it is discarded. This is the primary session
   validity check.
2. **Realized acceptance misses the configured mean** by more than 0.1 tokens.
   The knob did not take effect and the point is not what it claims to be.
3. **The speculative section is empty** on a speculative arm. See RUNBOOK
   Scenario 9; the run measured nothing.
4. **The load generator did not complete** its full request set, or completed
   it in a materially different wall time than sibling runs.
5. **GPU energy is absent** or the sampler warned.

Discarded runs are recorded in the results with their discard reason. They are
not silently re-run.

## Falsification criteria — declared in advance

**On the crossover.** The hypothesis is that a crossover exists within
L in [1, 6]: below it, speculation costs more energy per committed token than
the baseline; above it, less. If energy per committed token is monotonically
above baseline across the whole sweep, there is no crossover on this hardware
and workload, and that is the result. If it is below baseline everywhere,
likewise. Neither outcome is a failed campaign.

**On the instrument (D7).** The scalar knob is a sufficient instrument if the
crossover point is the same in both arms. Declared threshold: if the crossover
L differs between arms by more than 0.25 — one densification step — the scalar
knob alone is reported as insufficient and the geometric arm is retained in
later campaigns. If it differs by less, the scalar knob is validated and the
geometric arm is dropped.

If the coarse sweep shows no crossover, the instrument question is not
answered by this campaign and is reported as unanswered rather than assumed.

## Second stage

The five-point sweep locates the interval containing the crossover. A second
pass densifies at 0.25 steps within that interval only, in both arms. Same
protocol, same discard criteria, seed re-recorded.

If stage one is discarded, stage two does not run.

## What this campaign cannot answer

- Synthetic acceptance is not model acceptance. This measures how energy
  responds to acceptance with everything else held fixed; it does not say what
  acceptance any real draft model achieves (ADR-016 D7).
- One target/draft pair, one workload, one device. The crossover point is not
  claimed to transfer.
- Every limit ADR-012 states about device-level NVML energy holds unchanged.
