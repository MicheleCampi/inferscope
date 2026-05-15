# ADR-004: Report Metrics and Output Format

- **Status**: Accepted
- **Date**: 2026-05-15
- **Deciders**: Michele Campi

## Context

`is-report` is the layer that turns the raw signals captured by
`is-probe` and `is-sysmon` into a presentable report. Three
decisions need to settle before report code is written: which
metrics to derive, which aggregations to compute on the resource
timeline, and the shape of the output the tool emits.

By ADR-002 the probe stores only raw timestamps: every
per-token metric (time-to-first-token, inter-token latency,
tokens-per-second) is computed in this layer rather than at
collection time. By ADR-003 the sysmon stores raw kernel values:
RSS in bytes and CPU time in scheduler jiffies, converted to
presentable units here. The question is what to compute and how
to display it, not where to compute it.

A profiler's value lives in the metrics it reports. Picking the
wrong ones produces numbers that are technically correct but
useless for understanding what the engine is doing. Picking the
right ones makes the report read like a diagnosis.

## Decision

### Timing metrics derived from `RequestTiming`

The report carries the following derived timing metrics:

  - `token_count` — `tokens.len()`.
  - `ttft_ns` — time-to-first-token, equal to `tokens[0].elapsed_ns`
    if any token arrived, otherwise unset.
  - `total_latency_ns` — equal to `RequestTiming.total_ns`.
  - `tokens_per_second` — the generation rate during streaming,
    not including the TTFT. Computed as
    `(token_count - 1) / (tokens.last().elapsed_ns - tokens[0].elapsed_ns)`,
    in seconds. The TTFT is excluded deliberately: it measures
    request setup and prefill, not generation speed. Mixing the
    two produces a number that looks like "tokens per second" but
    is governed by whichever is larger.
  - Inter-token latency distribution — mean, p50, p95, p99, and
    max of the consecutive deltas
    `tokens[i+1].elapsed_ns - tokens[i].elapsed_ns`.

Percentiles on inter-token latency are computed from the sorted
delta list using nearest-rank with rounding up. With small
`token_count` p99 is essentially "the maximum"; the report shows
the sample size next to the percentiles so a reader can weight
them appropriately.

### Resource metrics derived from `ResourceTimeline`

  - RSS — `min`, `max`, `mean`, `final` in bytes. `final` is the
    last sample's value: useful for spotting whether memory grew
    during the request or stabilised.
  - CPU utilisation — average CPU consumption during the wall
    clock window of the request, expressed as a percentage that
    can exceed 100 on multi-threaded processes. Computed as
    `(total_jiffies_delta * 100 / clk_tck) / wall_seconds`, where
    `total_jiffies_delta` is the sum of user and system jiffy
    deltas between first and last sample, `clk_tck` is the
    system's `_SC_CLK_TCK`, and `wall_seconds` is the elapsed
    time between first and last sample. Reported as a single
    `mean` value; `peak` is not reported in v0.1.0 because per-
    sample CPU rate requires diffing adjacent samples and adds
    noise at the chosen 50 ms sampling rate.
  - Thread count — `min` and `max` over the timeline. If they
    differ, the engine grew or shrank its worker pool during
    the request.

If the resource timeline is empty (a request shorter than one
sampling period), the resource section of the report is omitted
rather than filled with placeholders.

### Output formats

Two parallel formats:

#### Text format

Targeted at terminal reading by a human. Plain ASCII, no colour,
fixed-width tables organised in three sections — probe summary,
inter-token latency, resource usage. The shape is intentionally
conservative: no graphs, no Unicode block characters, output that
copies cleanly into a code block or an issue tracker.

#### JSON format

Targeted at programmatic consumption. The JSON document carries
both the raw signals and the derived metrics:

  - The raw `RequestTiming` and `ResourceTimeline` (so a consumer
    can recompute metrics differently, or feed the data to a
    different visualisation).
  - The derived metrics as a sibling field.

Schema stability: v0.1.0 of the report JSON is best-effort. A
later version of inferscope may add fields (additive, non-
breaking) or restructure them (breaking). When v0.1.0 is tagged
the schema is frozen and a deprecation policy is added.

## Consequences

### Positive

- **Right metric for the right question.** `tokens_per_second`
  excluding TTFT measures generation speed, the thing a user
  asking "is this engine fast?" actually cares about. TTFT is
  reported separately for the orthogonal question "how long until
  I see something?"
- **Raw signals shipped alongside derived metrics.** The JSON
  output carries both, so the report does not need to be the
  final word. A user who disagrees with how a metric is computed
  can recompute it from the raw data without re-running the
  probe.
- **Honest about sample size.** With small token counts the high
  percentiles are weak signals. The report displays the count
  next to the percentiles so a reader is not misled by a "p99"
  computed from twelve samples.
- **Conservative text output.** Plain ASCII tables copy cleanly
  into issues, pull requests, blog posts. The output is the
  artifact, and it travels.

### Negative

- **CPU peak not reported in v0.1.0.** A user asking "what was
  the highest CPU usage during the request?" cannot answer that
  from the v0.1.0 report. Adding per-sample CPU rate computation
  is plausible for v0.2 once the right de-noising approach is
  picked; for now we report the mean only and call this out in
  the README.
- **Percentiles on small N are weak signals.** The report
  surfaces the sample size next to the percentiles, but a reader
  in a hurry may still read "p99" as if it were trustworthy.
  Acceptable trade-off: percentiles are the standard idiom in
  this space, omitting them would be confusing.
- **CLK_TCK is read at report time.** The conversion from
  jiffies to seconds depends on `_SC_CLK_TCK`, which is queried
  once when rendering. If the report is rendered on a different
  machine from the one that produced the timeline — and the two
  have different CLK_TCK values, which is rare but possible —
  the CPU utilisation will be miscomputed. The JSON output
  preserves the raw jiffies for re-computation on the correct
  host. The README documents the assumption.

## Alternatives Considered

### Reporting `tokens_per_second` from request start including TTFT

Rejected. The resulting number conflates two distinct phenomena:
the time the engine needs before the first token (a function of
prompt length, prefill speed, and KV cache state) and the rate
at which it generates subsequent tokens (a function of memory
bandwidth and the per-token compute). When TTFT is comparable to
or larger than the generation phase the combined rate is
governed by TTFT alone and ceases to mean what its name implies.
Reporting both metrics separately is more honest and more
informative.

### Reporting CPU peak from per-sample diffs

Rejected for v0.1.0 because at 50 ms sampling and 100 Hz CLK_TCK,
adjacent samples diff to either zero or a few jiffies depending
on whether a tick boundary crossed. The resulting per-sample
"CPU rate" is dominated by quantisation noise. Reporting it as
a peak invites readers to draw conclusions from numbers that do
not reflect reality. A future revision can either raise the
sampling rate or apply a windowed smoothing; either choice is a
v0.2 concern.

### One JSON document per artifact (probe vs sysmon vs report)

Rejected. Producing three JSON files for one probe run pushes
correlation work onto the consumer. The single combined document
keeps raw signals and derived metrics together; if a consumer
wants only one piece they extract the relevant sub-object. A
single artifact is easier to attach to a bug report or paste
into a notebook.

### Coloured terminal output

Rejected for v0.1.0. Colour helps interactive viewing but breaks
the "output is the artifact" property: pasting a coloured report
into an issue tracker turns into ANSI escape soup. v0.1.0 emits
plain ASCII; a later version can add a `--color` flag whose
default is `auto`.
