# ADR-010: Energy Consumption and Efficiency Metrics

- **Status**: Accepted
- **Date**: 2026-06-20
- **Deciders**: Michele Campi

## Context

ADR-005 brought GPU sampling into inferscope: instantaneous power
(milliwatts) and utilisation, sampled via NVML alongside the `/proc`
resource loop, joined on the same timeline. ADR-007 split those readings
per device. Together they answer "how hard is each GPU working, and how
much power is it drawing right now."

They do not answer the question the inference market now asks first. At
GTC 2026 NVIDIA reframed the AI data centre as a power-limited token
factory, where the metric that governs revenue is tokens per watt —
useful output produced per unit of energy under a fixed power ceiling.
Whatever one makes of the framing's commercial motive, the underlying
engineering quantity is real and was already half-present in inferscope:
the tool samples power and counts tokens, but reports them as two
separate truths and leaves the ratio — the efficiency — to the reader.

This ADR closes that gap. It settles how inferscope measures energy (not
just instantaneous power) and which efficiency metrics it derives from
energy and token throughput. It is the natural successor to ADR-005: that
ADR brought power in; this one integrates power over time into energy,
and turns energy plus tokens into efficiency.

Four decisions need settling: what to read for energy, which derived
metrics to expose, how to aggregate across devices, and — explicitly —
what inferscope does *not* measure, so the numbers are never read for
more than they are.

## Decision

### Energy source: NVML counter primary, integral fallback

NVML exposes `nvmlDeviceGetTotalEnergyConsumption`, returning total energy
in millijoules since the driver was last reloaded, on Volta and newer.
inferscope reads this counter at the start and end of the measurement
window and takes the delta. This is the primary source.

A feasibility check on an A10 (driver 580.105.08) confirmed the counter
is present, monotonic, and dimensionally correct: over a ~5.5s idle
window the counter delta (51.5 J) and a trapezoidal integral of the
power samples (56.0 J) agreed within ~8% — a ratio of 0.92. The residual
is expected and instructive: the hardware energy counter integrates at
high frequency, while the manual integral approximates the power curve
from samples taken every 500 ms. The counter is therefore the *more*
accurate of the two, not the less, and is preferred.

When the counter is unavailable — pre-Volta hardware, or any device that
returns `NVML_ERROR_NOT_SUPPORTED` — inferscope falls back to a
trapezoidal integral of the power samples it already collects per
ADR-005. The fallback is explicitly second-best and is flagged as such
in the report, so a consumer never confuses an integrated estimate with
a counter reading.

### Derived metrics: an efficiency family, not a single number

inferscope exposes a small family of energy and efficiency metrics rather
than tokens-per-watt alone. Reporting only the headline metric would hide
the quantities it is built from and make the number harder to trust.

- `energy_joules` — total energy over the measurement window (counter
  delta, or integral fallback; the source is recorded).
- `energy_per_token_mj` — millijoules per output token, the real unit
  cost of generation.
- `tokens_per_joule` — the inverse, expressed as efficiency.
- `tokens_per_watt` — the headline metric, computed as output
  tokens-per-second divided by mean power in watts.

A precise note, recorded here because it is easy to get wrong:
tokens-per-watt is dimensionally (tokens/second) / watt = tokens /
(watt·second) = tokens / joule. So `tokens_per_watt` and
`tokens_per_joule` are the *same physical quantity* expressed two ways.
inferscope reports both because the two framings serve different readers
— "tokens per watt" is the term the market uses, "tokens per joule" makes
the energy basis explicit — but they are not independent signals, and the
report says so.

### Multi-device aggregation

Per ADR-007 inferscope keeps per-device readings. For energy, the
window total is the sum of the per-device counter deltas; tokens are the
run's output tokens (one model, possibly sharded across devices). The
report carries both the aggregate efficiency and the per-device energy,
so an asymmetric multi-GPU run (the case ADR-007 exists to expose) shows
which device spent the energy, not just the total.

### What inferscope does not measure

The efficiency numbers are GPU-domain, and the report states this. They
do not include host CPU power, system memory, networking, cooling, or
power-supply overhead. The tokens-per-watt an AI-factory pays at the wall
— rack-level, including all of the above — is larger than what inferscope
reports, by a margin that depends on the deployment. inferscope measures
the GPU's contribution to that figure, which is the dominant but not the
whole term.

One case is called out specifically as a known limitation and an open
door. On Grace-Blackwell / Grace-Hopper superchips, the GPU and an ARM
Grace CPU share one module and one power envelope (e.g. ~2.7 kW for a
GB200 module). NVML is GPU-centric: it reports the Blackwell/Hopper GPUs,
not the Grace CPU, and not the combined module. On such hardware
inferscope's energy figure is the GPU-domain term, not the module-domain
term, and the gap is material. Closing it — a power-domain-aware model
that can attribute energy at the module level on superchip hardware — is
deferred to a future ADR, to be designed and validated when superchip
hardware (a GH200 is the cheapest accessible instance of the pattern) is
available to test against. The door is left open the way ADR-003 left it
open for GPU sampling: the metric types and the report schema do not
assume the GPU is the whole power domain.

## Consequences

### Positive

- inferscope moves from a performance-and-diagnosis tool to one that also
  answers the efficiency question the inference market now leads with,
  using a measurement (cumulative energy) it did not expose before.
- The headline metric is grounded: every tokens-per-watt traces to a
  hardware energy counter and a token count, both recorded, regenerable.
- The efficiency family makes the CUDA-graphs finding expressible in the
  market's own terms — a configuration that costs tokens-per-watt under
  saturation — without new instrumentation.
- The stated limits make the numbers safe to cite: GPU-domain, not
  wall-power, said plainly.

### Negative

- The fallback integral and the counter are not interchangeable in
  precision; consumers must read the recorded source. Mitigated by
  flagging it explicitly in the report.
- tokens-per-watt and tokens-per-joule are the same quantity; exposing
  both risks the appearance of two signals where there is one. Mitigated
  by stating the identity in the report and here.
- The superchip limitation means inferscope's efficiency figure is
  incomplete on exactly the hardware (GB200-class) the market cares most
  about. Accepted for now, with the open door above.

## Alternatives Considered

### Integral of power samples as the primary source

Rejected as primary. The A10 check showed the hardware counter is more
accurate than a sampled integral, and it is simpler (two reads, one
delta, no accumulation loop). The integral remains the fallback for
hardware without the counter.

### tokens-per-watt only, no family

Rejected. Reporting the ratio without the energy and per-token terms it
derives from would make it harder to trust and impossible to debug. The
family costs little and grounds the headline.

### Synchronised per-request energy

Best-practice energy measurement (ML.ENERGY) synchronises CPU and GPU to
attribute energy to a single step. inferscope profiles a server under
load over a window, not a single kernel, so window-level delta is the
right granularity. Per-request energy attribution is deferred; the report
notes that energy precision is window-level.

### Waiting for superchip hardware before shipping energy metrics

Rejected. The Level-1 metrics (energy, efficiency family) are fully
validatable on the Ampere/Ada/Hopper hardware already in use. Coupling
their release to superchip access would delay a complete, honest feature
for a future extension that is explicitly scoped as separate.
