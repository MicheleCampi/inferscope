# Status in full

Moved from the README unchanged; links adjusted for this folder.

**v0.5.0 released (2026-07-29).** Pre-1.0: the report schema still
changes between releases (see [`CHANGELOG.md`](../CHANGELOG.md)). It runs
on CPU and NVIDIA GPU. GPU support is NVIDIA-only today; all GPU and energy
features are gated behind the `gpu-nvidia` feature flag.

- **CPU foundation** (v0.1): token timing capture, `/proc`-based
  process resource sampling, derived metrics, text/JSON reporting.
- **NVIDIA GPU sampling** (v0.2): per-device utilization, memory,
  and power via NVML.
- **Energy & efficiency** (v0.4): total energy from the NVML
  hardware energy counter, with derived tokens-per-joule and
  tokens-per-watt. Validated end-to-end against a real llama.cpp
  workload on an NVIDIA A10 — see
  [`validation-results/`](../validation-results/) for the captured
  evidence.
- **Attribution** (v0.4): KV-cache hit rate scraped from a
  Prometheus endpoint (ADR-011), per-phase energy split
  prefill vs decode with the divergence between two apportionments
  as the signal (ADR-012), and per-step attribution across agentic
  trajectories (ADR-013). Per-step energy is measured on an A10
  against vLLM. **No KV-cache hit rate has been measured against a real
  vLLM endpoint.** Until 2026-08-02 the schema named the series as
  vLLM's source registers them rather than as its endpoint exposes them,
  so every scrape against real vLLM failed and only the simulator
  answered; the A10 evidence of 2026-07-21 carries zeros for that
  reason. Fixed in `cd0ece6`, and still unmeasured after the H100 run
  of 2026-09-05 for a second reason: that campaign drives load
  externally and attaches with `--sample-only`, a path that spawns the
  phase and speculative scrapes but not the KV one. Recorded as a gap
  in ADR-016, not closed — see the ADR-011 postscript.
  [`validation-results/`](../validation-results/) states the bounds of
  each run.
- **Multi-engine** (v0.5): SGLang is read alongside vLLM (ADR-014).
  The two do not expose the same quantity — vLLM counts cache hits
  truncated to a block boundary, SGLang counts exact tokens at its
  default page size — so a hit rate now carries the accounting that
  produced it into the report, and the rendered text says which.
  The engine is declared with `--engine`, never inferred from the
  scrape body. Parser and schema selection are validated against
  fixtures transcribed from SGLang's own collector source and
  cross-checked against its tests, and — since 2026-09-10 — against a
  live SGLang 0.5.19 server on an A10: hit rate 0.9859 over a window of
  420 hits in 426 queries, accounting `exact_tokens` at the page size the
  server reported for itself. Two things that run caught and nothing else
  would have: SGLang serves `/metrics` only when started with
  `--enable-metrics`, returning a plain 404 otherwise; and a
  single-request probe cannot derive a rate from an engine that publishes
  its counters on request completion, because the window closes with the
  request that would have moved them. Evidence in
  [`validation-results/adr-014-a10-sglang/`](../validation-results/adr-014-a10-sglang/).
  The transcription was itself wrong once: it omitted `is_streaming`,
  which SGLang appends to the two token counters and to those alone,
  so a server answering both streaming and non-streaming requests
  emits each counter as two lines. Reading the first — which is what
  a single-line aggregation does — reported a part as the whole and
  inflated the hit rate. Found by re-reading the collector on
  2026-09-05, fixed with an aggregation that sums the family, and
  pinned by a test that fails if the old form comes back. vLLM was
  checked for the same defect at source and does not have it: its
  counters carry no label the schema does not already select on.
- **Speculative decoding** (ADR-016, on `main`, not yet tagged): the three vLLM speculative
  counters on the same clock as the energy sampler, so what rejected
  drafts cost can be read in joules rather than inferred from an
  acceptance rate. Measured on an H100 PCIe on 2026-09-05, with
  `synthetic_acceptance_rates` making acceptance an independent
  variable rather than a property of whichever draft model was to hand
  — eleven runs, baselines opening and closing the session 0.13%
  apart, realized acceptance length matching the configured value on
  every point. **No energy crossover exists in the swept range.**
  Speculation costs less per committed token than not speculating at
  every acceptance length, including at zero acceptance: 652880 draft
  tokens computed, none accepted, and the run still committed its
  tokens at 0.897x the baseline. Figures, the mechanism, and the
  source-level checks behind the attribution are in
  [`validation-results/adr-016-h100-spec/RESULTS.md`](../validation-results/adr-016-h100-spec/RESULTS.md).
  One target/draft pair, one workload, one device.
