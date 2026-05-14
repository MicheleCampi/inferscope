# ADR-002: Token Timing Representation

- **Status**: Accepted
- **Date**: 2026-05-14
- **Deciders**: Michele Campi

## Context

The probe (`is-probe`) receives generated tokens from an inference
engine as a stream. To profile the engine it must record *when*
each token arrives. The shape in which these arrival times are
stored in `is-core` — the shared, serializable data layer — has
consequences for serialization, for precision, and for how much
analysis flexibility survives into later stages.

Three representations were considered.

The first is to store an absolute `std::time::Instant` per token.
`Instant` is the natural type for measuring elapsed time inside a
process, but it is deliberately opaque and not serializable: it is
monotonic and tied to an unspecified epoch, so it carries no
meaning outside the process that produced it. Since `is-core`
exists specifically to hold types that serialize to JSON, an
`Instant` cannot live in its types.

The second is to store pre-computed inter-token deltas: for each
token, the time since the previous one. This is convenient because
inter-token latency is a metric we want — but it discards the raw
signal. Any metric not anticipated at collection time (for
example, time from request start to the Nth token) becomes
unrecoverable.

The third is to store, per token, the elapsed time since the
request was sent, as an integer count of nanoseconds.

## Decision

Token arrival times are stored as **`u64` nanoseconds elapsed
since the request was sent**.

`is-probe` captures one `Instant` at the moment the request is
sent and keeps it as an internal detail. For each arriving token
it records `start.elapsed().as_nanos()` as a `u64` into the
`is-core` type. The `Instant` never crosses the crate boundary;
the shared type sees only integers.

Derived metrics are computed downstream, in `is-report`, from this
raw signal: time-to-first-token is the first token's timestamp,
inter-token latency is the difference between consecutive
timestamps, total latency is the last timestamp, and
tokens-per-second is the token count over the elapsed duration.

## Consequences

### Positive

- **Trivially serializable.** A `u64` needs no custom serde
  handling, unlike `Instant`.
- **Meaningful across process boundaries.** "458 ms since request
  start" is a sensible value in a JSON file or a report; an
  `Instant` is not.
- **No precision loss.** Nanoseconds in a `u64` span roughly 584
  years of duration, far finer and longer than anything inference
  profiling requires.
- **Raw signal preserved.** Because nothing is pre-computed, any
  metric expressible from per-token arrival times can still be
  derived later. The data layer does not constrain the analysis
  layer.
- **Clean separation.** `is-core` holds raw data; `is-report`
  interprets it. The data does not know how it will be analyzed.

### Negative

- **Consumers do arithmetic.** Code reading these timings must
  subtract to obtain inter-token latency rather than reading a
  ready-made metric. The arithmetic is trivial, and the gain in
  flexibility is worth it.
- **Relative, not wall-clock.** These timestamps are relative to
  request start, not absolute calendar time. This is intentional
  — wall-clock time is irrelevant to profiling — but it means
  correlating with external wall-clock logs would need the
  request start time recorded separately. No current requirement
  needs this.

## Alternatives Considered

### Absolute `Instant` per token

Rejected for `is-core` types: `Instant` is not serializable and
carries no meaning outside its originating process. It remains the
right type *inside* `is-probe` during measurement; it simply does
not cross into the shared data layer.

### Pre-computed inter-token deltas

Rejected: storing deltas discards the raw signal. A metric not
foreseen at collection time cannot be recovered. Storing raw
per-token timestamps and deriving deltas downstream keeps every
future metric reachable at negligible cost.
