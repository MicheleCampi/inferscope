# Stage one — results

- **Run**: 2026-09-05, 11 runs, ~93 minutes
- **Hardware**: 1x H100 PCIe (80GB), driver 580.105.08, vLLM 0.28.0
- **Target / draft**: Qwen2.5-3B-Instruct / Qwen2.5-0.5B-Instruct, k=5
- **Session**: valid — every discard criterion passed

## Validation

Baseline drift 0.13% against a 5% tolerance (43030674 mJ opening, 42975418 mJ
closing). Realized acceptance length matched the configured value on all nine
points, including the geometric arm: 1.9947 for 2.00, 2.9918 for 3.00, 3.9958
for 4.00, 5.0025 for 5.00.

## Energy per committed token

Baseline, no speculation: **328.744 mJ/token**.

| run | realized L | mJ/token | vs baseline |
|---|---|---|---|
| L1.00-minvar | 1.00 | 294.855 | 0.897x |
| L2.00-geom | 1.99 | 263.631 | 0.802x |
| L2.00-minvar | 2.00 | 267.035 | 0.812x |
| L3.00-geom | 2.99 | 252.725 | 0.769x |
| L3.00-minvar | 3.00 | 249.929 | 0.760x |
| L4.00-geom | 4.00 | 243.737 | 0.741x |
| L4.00-minvar | 4.00 | 243.225 | 0.740x |
| L5.00-geom | 5.00 | 235.296 | 0.716x |
| L5.00-minvar | 5.00 | 237.746 | 0.723x |

## No crossover, and the reason is the interesting part

The hypothesis was a crossover inside L in [1, 6]: speculation costing more
per committed token below some acceptance length, less above it. There is
none. Speculation is cheaper at every point, and the falsification criterion
declared before the runs says that outcome is the result, not a failure.

What makes it a finding rather than a null is the L=1.00 point. There the
acceptance vector is all zeros: 130576 rounds, 652880 draft tokens proposed,
**zero accepted**. Every draft was computed and thrown away. That run still
committed its tokens at 0.897x the baseline's energy.

Speculating and accepting nothing is 10% cheaper per committed token than not
speculating at all.

The mechanism is in the shape of the forward pass rather than in the
speculation succeeding. `uniform_decode_query_len` is 1 without a speculative
config and 1+k with one — "a decode step submits one query for the newly
sampled token plus one for each draft token" (config/vllm.py:632). Decode is
memory-bandwidth-bound: the cost of streaming the weights is paid once per
forward regardless of how many queries ride along. Verifying six tokens
amortises that cost over six queries even when five of them are discarded.

Acceptance rate and tokens-per-second cannot see this. Both are throughput
figures, and at zero acceptance both say speculation did nothing.

## Why the comparison is attributable

Checked at source before drawing the conclusion, because the L=1.00 figure is
the kind of result that is usually an artefact:

- **The denominator is clean.** Generated tokens are constant within 1.32%
  across all eleven runs (130003 to 131723, a 1.32% spread). The counter does not include
  draft tokens.
- **The scheduler budget is the same.** `_set_max_num_scheduled_tokens`
  (config/vllm.py:1909) sets it equal to `max_num_batched_tokens` and never
  subtracts the drafting delta. `draft_model` asks for 1 additional slot, not
  k (config/speculative.py:1814), against a budget of 8192.
- **The load was identical in fact, not just nominally.** Both arms completed
  648 of 1600 requests in 3:46. The arrival rate was the bottleneck, not the
  server, so mean concurrency and batch shape were the same.
- **The counters are internally consistent.** `draft_tokens / drafts` is
  exactly 5 in every speculative run.

## Limits

- **One target/draft pair, one workload, one device.** H100 PCIe at 2.0 TB/s
  HBM. The crossover question is bandwidth-sensitive by the very mechanism
  above; SXM at 3.35 TB/s would shift the balance and is not covered.
- **The captured CUDA graph sizes were not compared between arms.** The
  closing baseline captured 51 sizes up to 512; the speculative servers' logs
  were overwritten per run and the instance was released before they were
  copied. The difference in query length is the speculation itself rather than
  a configuration artefact, but the comparison was not made.
- **Synthetic acceptance is not model acceptance.** This measures how energy
  responds to acceptance with everything else fixed. It says nothing about
  what acceptance a real draft model achieves.
- **The instrument question (D7) is unanswered.** Comparing the two arms
  required a crossover to compare. Without one, whether the scalar knob is a
  sufficient instrument is not decided by this campaign, and is reported as
  unanswered rather than assumed.
- Every limit ADR-012 states about device-level NVML energy holds unchanged.

## What follows

Stage two — densifying around a crossover — does not run: there is no
crossover to densify around. The protocol says so.

The open question this raises instead is where the L=1.00 effect goes as
bandwidth changes, and whether it survives on a part where decode is less
bandwidth-starved. That is a different campaign, not a second stage of this
one.
