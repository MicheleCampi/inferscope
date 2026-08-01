# ADR-015: Cost Attribution per Trajectory Step

- **Status**: Accepted
- **Date**: 2026-08-01
- **Supersedes**: none
- **Related**: ADR-010 (energy), ADR-012 (per-phase), ADR-013 (trajectory), ADR-014 (schema provenance)

## Context

Inferscope reports energy. The people who operate inference platforms
report money. The two are related by a rate, and the rate is not
something this tool can measure.

ADR-013 already produces, per trajectory step, an exact energy figure
and an exact window in report coordinates, with the reconciliation
`steps + unattributed == total` holding on integer accounting. What it
does not answer is the question an operator actually asks: **which step
of an agent trajectory spends the money**.

### There is no dollar counter

NVML exposes a joule counter. No hardware exposes a dollar counter.
Every monetary figure in every cost tool is `rate x usage`, where usage
is measured and the rate is declared from an external price list. This
is not a shortcut taken here; it is the only available shape. The
industry tooling that attributes cluster spend works the same way:
prices come from a provider API or an operator-supplied list, and get
multiplied by measured resource-seconds.

The consequence for this project is a naming obligation, not a design
compromise. A report whose central claim is "measured" must not grow a
field that looks measured and is not.

### Two bases, and they are not additive

There are two distinct things a step can cost:

- **Occupancy** — the node is rented by the hour and is billed whether
  or not it computes. `duration x price_per_hour`.
- **Energy** — the hardware is owned and the electricity is metered.
  `joules x price_per_kwh`.

On a rented node the energy is already inside the hourly price, so
adding the two double-counts. On owned hardware occupancy is not a
cash cost at all; the corresponding figure is amortisation, which this
tool has no basis to compute.

The two are not close. For an illustrative step of 4 s on a node rented
at $1.00/h, occupancy is $0.00111; the same step drawing 1 kJ at
$0.20/kWh is $0.000056, roughly twenty times smaller. These figures are
arithmetic on chosen inputs, not measurements, and are here only to
show that the choice of basis changes the answer by more than the
precision anyone would argue about. No measured cost figure appears in
this ADR; see "Why the 2026-07-21 evidence is not used" below.

### What the existing structure already guarantees

Two properties of ADR-013 are load-bearing here and are inherited
rather than re-established:

- `StepMetrics` carries `start_elapsed_ns` and `end_elapsed_ns`, so
  step duration is exact and requires no new instrumentation.
- Kept steps are disjoint by construction: an overlapping step is
  dropped with `DropReason::OverlapsPrecedingStep`, guarded by
  `overlapping_step_is_dropped_and_never_double_counted`. Therefore
  the sum of attributed durations can never exceed the run duration,
  and attributed cost can never exceed cost paid.

### What it does not guarantee

The energy residual and the time residual are different numbers.
`unattributed_energy_mj` excludes inter-sample segments that straddle a
step boundary, because a segment belongs to at most one window. Time
has no such loss: the run window is fully tiled by "inside a step" and
"outside every step". The two residuals are not convertible and must
not be reported as one figure.

## Decision

### D1 - Cost lives outside `Report`, and the separation is structural

`Report` gains no cost field. Cost derivation is a pure function over
an already-derived `TrajectoryMetrics` plus declared rates, returning
its own type. A serialized report therefore continues to contain only
measured quantities and their derived ratios.

This is stronger than documenting the distinction: a reader holding a
report cannot mistake a declared rate for a measurement, because the
declared rate is not in the file.

### D2 - `CostBasis` is explicit and single-valued

```rust
pub enum CostBasis {
    /// Node rented by wall-clock time; energy is already priced in.
    Occupancy,
    /// Hardware owned; electricity metered separately.
    Energy,
}
```

One basis per derivation. There is no "total" that sums them, and no
default: the caller states which cost they are talking about. A
consumer that wants both computes both and presents them as two
answers to two different questions.

### D3 - Rates are inputs with no embedded price list

Rates are supplied per invocation. No provider price table ships in the
crate. A hardcoded list is a fossil surface: it is correct on the day
it is written, silently wrong afterwards, and nothing in CI can detect
the drift.

The rate travels into the output alongside the figure it produced, so a
result is never separable from the assumption that made it.

### D4 - The run window is the integration window, not a new concept

Run duration is taken from the same timeline that provides the energy
integration basis, so occupancy and energy describe the same interval.
No second notion of "the run" is introduced.

### D5 - The temporal residual is its own figure

`unattributed_duration_ns` is reported separately and is never derived
from `unattributed_energy_mj`. On the occupancy basis this residual is
paid: the node is powered during model load, between steps, and while
the driver thinks. Reporting attributed cost without it would understate
what the run cost.

### D6 - `usd_per_million_tokens` is `Option`, and absence is not zero

`None` for tool steps, for steps with no scraped phase timeline, and
for zero generation tokens. A step that produced no tokens has no cost
per token; it does not have a cost per token of zero. This follows the
same rule as `tokens_per_joule` in ADR-013.

### D7 - The measurement contract is stated, not implied

The following are outside what this derivation can see, and are
recorded here so that a number carrying this ADR carries them too:

1. **The run window is not the invoice.** Provisioning, model load and
   post-run idle are billed and are not in the window. The unresolved
   27 s vs 96 s engine-init finding is real money this figure omits.
2. **Billing granularity.** Providers bill by the minute or the hour.
   A per-step marginal cost is an attribution, not an invoiced amount.
3. **Multi-tenancy.** If the node serves concurrent traffic, charging
   the whole hourly rate to one trajectory overstates it, and the
   report contains nothing that would reveal this. Single-tenant
   profiling runs are the validity domain.
4. **No invoice reconciliation.** Attribution is not compared against
   what was actually paid. That comparison is the discipline's own
   gold standard and is not performed here.

The defensible claim is cost attribution for profiling runs. It is not
cost management, chargeback, or a billing system.

## Consequences

### Positive

- Answers the question the energy metrics could not: which step of a
  trajectory spends the money, at a granularity no cluster cost tool
  offers, because per-step attribution requires the driver-boundary
  join that ADR-013 already performs.
- Requires no GPU and no new instrumentation of its own: the inputs are
  quantities ADR-013 already derives. It does require a report that
  serializes the raw timelines, which is a property of how a run is
  captured, not of this derivation.
- The exactness inherited from ADR-013 carries over: disjoint windows
  mean attributed cost never exceeds cost paid.

### Negative

- Introduces a declared quantity into a project that has so far only
  published measured ones. D1 and D3 confine it; they do not remove it.
- Two bases mean a consumer can pick the wrong one. The enum makes the
  choice visible but cannot make it correct.
- The occupancy figure is only as meaningful as the single-tenant
  assumption behind it, which the tool cannot verify.

## Why the 2026-07-21 evidence is not used

The A10 + vLLM run that validated ADR-013 is the obvious candidate to
carry the first cost figure, and it cannot. The reasons are recorded
here because they are the reasons a later run must avoid.

- **The GPU timeline was not serialized.** `gpu_timeline` is absent
  from that report, so `derive_trajectory` cannot be re-run against it
  at all: it withholds without an energy basis. The per-step `energy_mj`
  values in the file are the derived output of the build of the day,
  not something any command in this repository regenerates today.
- **The per-step token counters predate the `bracket()` fix.** As
  captured, prompt deltas read zero on all five steps and generation
  summed to 151 against 168 over the same window. That file is kept
  precisely because it exposed the defect; building a published number
  on top of it would use an artifact for the opposite of its purpose.
- **The sampling window is a session parameter.** 150 s were chosen for
  a trajectory that ran 6.2 s. Any run-level cost derived from it would
  be dominated by that choice, and a figure that moves with the
  operator's `--duration-secs` is not a property of the workload.
- **Tool steps resolved no counter delta.** At 0.2 s they are shorter
  than the counter sampling period. `samples_in_window` records this
  per step, and a cost per token over such a step would be arithmetic
  on absence.

The first measured cost figure therefore comes from a run captured for
this purpose: raw timelines serialized, the corrected bracketing in
place, and a sampling window sized to the trajectory rather than to the
session. Until then this ADR is a design decision with no number
attached, which is the honest state to publish it in.

## Validation boundary (VM vs GPU)

Nothing in this ADR touches a GPU. The derivation is arithmetic over
`TrajectoryMetrics`, which is already produced and already validated.

Validation is therefore in two parts:

- **Unit**, on the VM: conversion factors, the `Option` semantics of
  D6, the residual identity `attributed + residual == total` on the
  occupancy basis, and the property that attributed duration never
  exceeds run duration.
- **End-to-end, on existing evidence**: the derivation runs against the
  stored A10 + vLLM reports rather than against fixtures alone. The
  claim this supports is "derived from measured trajectories", and the
  measurement it rests on was made on real hardware; the arithmetic on
  top of it was not, and does not need to be.

## Alternatives Considered

**A CLI flag writing cost into the report.** Symmetrical with
`--steps-file`, and rejected: it puts a declared number inside the
artifact whose value is that everything in it is measured. The flag
remains available later as a convenience over a function that already
exists; the reverse order would not be recoverable.

**An embedded provider price list.** Convenient for one release and
wrong from the next. See D3.

**A single summed cost.** Would produce one number instead of two and
would be wrong on both deployment models: double-counting on rented
nodes, incomplete on owned hardware.

**Placing this decision on the operator instead.** The operator holds
placement authority but not the measurements: it has no notion of a
trajectory step, and would have to duplicate ADR-013 before it could
price one. The hourly rate is not an argument for the operator either,
since it is declared configuration in both places. Acting on cost is a
separate decision, and belongs in a separate ADR on that side.

## Postscript 2026-08-01: rates travel inside `CostBasis`

The D2 snippet above shows `CostBasis` as a payload-free enum, with
rates arriving separately per D3. The implementation carries the rate
inside the variant instead:

```rust
pub enum CostBasis {
    Occupancy { usd_per_hour: f64 },
    Energy { usd_per_kwh: f64 },
}
```

The two bases take rates in different units. Keeping them outside the
enum admits two states the design does not want: a basis whose rate is
missing, and a pair where both rates are supplied and one is ignored.
Neither is reachable once the rate is a field of the variant, and D3's
requirement that a figure never be separable from the assumption
behind it is enforced by the type rather than by convention.

This narrows D2 and D3; it does not reverse them. There is still one
basis per derivation, still no default, and still no price list in the
crate.

Two further points settled during implementation, recorded here
because a reader of the decision would otherwise have to infer them:

- **`run_duration_ns` is the node's span, not a per-device window.**
  On a multi-device node the timeline's first and last samples belong
  to one device, so the run span exceeds each device's own integration
  window by a fraction of a sample tick. This is intended under D4:
  occupancy is paid for the node, not for the union of per-device
  windows.
- **A pre-ADR-015 report prices as absent, not as free.** The two new
  fields carry `serde(default)`, so an archived report deserializes
  with `run_duration_ns == 0`. `derive_cost` withholds on the
  occupancy basis in that case rather than returning a zero cost,
  under the same rule as D6.

Implemented in `crates/is-report/src/cost.rs`. No dollar figure has
been measured; the validity domain in D7 is unchanged.
