# ADR-006: Process Tree Aggregation for Sysmon

- **Status**: Accepted
- **Date**: 2026-05-21
- **Deciders**: Michele Campi

## Context

ADR-003 fixed sysmon's contract to a single PID: read
`/proc/<pid>/status` and `/proc/<pid>/stat`, sample at a fixed
cadence, return one resource value per tick. The contract
worked correctly for v0.1.0 against llama.cpp in single-process
mode and for the v0.2.0 GPU validation on RTX L4 — the
binaries we tested expose their full workload through the PID
the user passes on the command line.

The wrapper-PID class of failure surfaced during the L4
validation itself. `llama-server` started with bash background
redirection (`./llama-server &`) leaves `$!` pointing at the
wrapper shell that forked the worker, not at the worker
process. Passing that PID to `inferscope --pid` produces a
report that says RSS 2 MiB, CPU 0%, 1 thread — values that are
factually correct for the wrapper but useless as a
characterisation of the inference engine. The same shape
appears with `uvicorn` (master spawns workers), `gunicorn`
(arbiter spawns workers), `vllm.entrypoints.openai.api_server`
(loader spawns engine), and any other server that uses
`fork()` as a startup pattern. The wrapper-PID failure is not
a llama.cpp quirk — it is the default shape of multi-process
Python and C++ servers on Linux.

Two prior commits closed the gap from below: `ad4ccd0`
documented the bash `$!` pitfall in the GPU validation
runbook, and `7f800ec` added a runtime warning that fires when
every sample shows RSS < 10 MiB, exactly 1 thread, and zero
CPU jiffies — the heuristic signature of a wrapper PID. The
combination is good operator ergonomics: the user is told
during the run that the sample is suspicious, and the runbook
tells them how to pick the right PID. Neither commit, however,
made the tool *compute the right answer* when handed the wrong
PID.

This ADR addresses the remaining gap: when the user opts in
via a new CLI flag, sysmon should aggregate the resource
metrics of the supplied PID together with those of its direct
children, producing a sample that characterises the real
workload regardless of which PID in the tree was supplied.

The empirical shape of the problem was verified during the
design of this ADR: a synthetic bash parent producing two
children (one persistent `sleep`, one transient subshell) was
inspected via `/proc/<parent>/task/<parent>/children`. The
kernel-maintained file is a single line of space-separated PIDs,
updated atomically per read. The transient subshell appeared in
the read, then disappeared on the subsequent `/proc/<child>`
access — a textbook race condition that any tree-walking
implementation must tolerate.

## Decision

Aggregation is opt-in, single-level, sequential, sum-based,
saturating, and failure-tiered. Each adjective is a deliberate
constraint with a rationale.

**Opt-in.** A new flag, `--include-descendants`, defaults to
`false` and a corresponding `SysmonConfig::include_descendants`
field defaults to `false`. Every existing call site keeps
producing the v0.2.0 behaviour exactly. The opt-in surface is
small: a builder method `SysmonConfig::with_descendants()` and
a single CLI flag. The aggregation path is not selected
heuristically — the user (or a script) makes the choice, and
the heuristic warning from `7f800ec` is what tells them to
make it.

**Single-level (direct children only).** Sysmon reads
`/proc/<pid>/task/<pid>/children`, which lists the kernel's
record of direct child PIDs of the given thread. It does not
recurse into grandchildren. The choice is grounded in the
process topology of the inference engines inferscope targets:
`llama-server` forks one worker (one level), `vllm` spawns
worker processes from a master (one level for most
deployments), `uvicorn` and `gunicorn` follow the same shape.
The marginal value of grandchild recursion is small for the
v0.2 target audience, and the complexity cost is real (a tree
walk must terminate, must deduplicate cycles, must handle
PID-namespace edge cases). v0.3 may revisit this if a
production deployment surfaces the need.

**Sequential, not parallel.** Reading `/proc/<pid>/status` and
`/proc/<pid>/stat` takes single-digit microseconds on a modern
Linux host. For the N <= ~10 children case that inference
engines produce, sequential reads complete in tens of
microseconds total. Parallelising via `tokio::join!` or
`JoinSet` would add task-scheduling overhead that exceeds the
I/O it parallelises. The code stays a flat loop, trivially
testable, and predictably ordered.

**Sum-based aggregation.** All four numeric fields in
`ResourceSample` are sums across the process group:
`rss_bytes`, `cpu_user_jiffies`, `cpu_system_jiffies`,
`thread_count`. The `elapsed_ns` is preserved as the parent's
value — that PID is the timestamp reference per ADR-003 and
must remain the anchor in the timeline. Sum is the right
aggregation for RSS (RAM held is RAM held, regardless of which
process holds it), for jiffies (total CPU work done by the
group), and for threads (total schedulable units alive).
Alternative aggregations — max, mean, parent-only — would all
lose information the user opted in to receive.

**Saturating arithmetic.** Every sum uses `saturating_add`.
The `u64` fields cannot realistically overflow on a Linux
system (the kernel itself bounds them as `u64`), but the `u32`
`thread_count` can in theory: a parent with 10 children, each
with hundreds of thousands of threads, could brush against
`u32::MAX`. Saturating is cheap and uniform; using
`checked_add` and propagating an error for a corner case the
user cannot meaningfully act on would be worse ergonomics.

**Failure-tiered behaviour.** Three failure paths exist, with
three different responses:

1. Reading the parent's `/proc/<pid>/status` or `stat` fails:
   the function returns `Err(SysmonError::Io)`. The parent PID
   is the user's anchor; if it is unreadable the sample is
   meaningless and the caller must know.

2. Reading `/proc/<pid>/task/<pid>/children` fails: the
   function returns the parent-only sample (the result of
   `sample_once`). The kernel may decline to expose this file
   in unusual namespace configurations, or the PID may have
   exited between the parent read and this read. The parent
   sample is still useful in either case and the per-tick
   tolerance of `sample_during` already handles a fully-failed
   tick gracefully.

3. Reading a specific child's `/proc/<child>` files fails:
   the child is silently skipped. The race condition observed
   in the empirical test (a transient child appearing in
   `children` then exiting before the follow-up read) is the
   normal case here. A permission error on one child should
   not poison the entire sample.

## Consequences

**Backward compatibility is complete.** No existing
`SysmonConfig` constructor changes its behaviour. The new
field has a default that preserves v0.1.0 / v0.2.0 semantics
exactly. No call site in the workspace constructs
`SysmonConfig` with literal struct syntax (verified with
`grep -r 'SysmonConfig {' --include='*.rs'`), so the additive
field cannot break downstream code.

**I/O cost grows linearly in N children.** Each tick now
performs 2 reads (parent status + stat) + 1 read (children
file) + 2N reads (each child's status + stat), where N is the
number of direct children at the moment of the tick. For the
expected N <= 10 the absolute cost is still well under a
millisecond per tick, far below the 50 ms default sampling
period. For pathological N (hundreds of children) the loop
could fall behind the ticker, at which point
`MissedTickBehavior::Skip` already handles the consequence:
samples are dropped rather than queued.

**Race conditions are tolerated silently.** A child can exit
between discovery (the read of the `children` file) and the
sample (the read of `/proc/<child>/*`). The per-child error is
absorbed and the rest of the aggregation proceeds. This
matches the existing per-tick tolerance of `sample_during` and
preserves the "best-effort" contract of the sampling layer.

**Reporting granularity is intentionally lost.** The
aggregated `ResourceSample` does not distinguish parent from
children. A report cannot say "the parent held 100 MiB, the
worker held 1 GiB" — only "the process group held 1.1 GiB".
For the v0.2.1 audience (operators trying to characterise an
inference engine) this is the right trade. A future revision
could carry per-PID detail in the JSON output behind a
verbosity flag, but the text report stays compact.

## Alternatives Considered

**Full recursive walk via `/proc` scan.** Instead of reading
`/proc/<pid>/task/<pid>/children`, scan every `/proc/*/stat`
and filter by `ppid` matching the target. Recurse to find
grandchildren and beyond. Rejected: high syscall cost
(thousands of reads on a busy host for one logical sample),
unbounded depth, and unnecessary for the target inference
engines whose process trees are at most one level deep.
Reconsider in v0.3 if a production deployment surfaces a
multi-level case.

**Parallel sampling via `tokio::join!` or `JoinSet`.** Spawn
one async task per child and join. Rejected on cost grounds:
the I/O parallelised is microseconds per syscall, while task
scheduling overhead is also microseconds. The sequential loop
is the same wall-clock cost with simpler control flow and
better observability under a debugger.

**PID auto-discovery via the endpoint's listening socket.**
Given `--endpoint http://127.0.0.1:8080`, parse `/proc/net/tcp`
to find which PID has port 8080 in `LISTEN` state, and use that
PID as the target — eliminating the wrapper-PID failure at the
source. Rejected for v0.2.1 scope: this would also eliminate
the `--pid` flag, which is a larger API change than this ADR
covers. Carried forward as a candidate for v0.3 alongside the
AMD GPU sampling work.

**Treat per-child read failure as fatal.** Reject the entire
tick if any child's `/proc/<child>/*` cannot be read.
Rejected: the race between `children` enumeration and
per-child sampling is the *normal* case, observed in the
empirical bash test that shaped this ADR. A fatal-on-race
policy would produce a flapping report on busy hosts. The
silent-skip policy preserves the value of every successful
read.

**Carry per-PID detail in the JSON output.** Add a
`per_pid_breakdown` field to `ResourceSample` so that the
JSON consumer can distinguish parent from each child.
Rejected for v0.2.1: changes the JSON schema (a public
contract per ADR-004), adds complexity to the aggregation
layer, and serves an audience that does not exist yet — no
v0.2 user has asked for per-PID detail. Reconsider in v0.3 if
demand surfaces.
