//! Sampling loop that reads /proc periodically and accumulates a
//! resource timeline.
//!
//! This is the only module in the crate that performs I/O. The
//! pure parsing logic lives in [`crate::parse`]; this module reads
//! file content from disk, hands it to the parser, and adds the
//! resulting sample to a growing timeline at a configured cadence.
//!
//! Per ADR-003 the timestamp on each sample is `elapsed_ns` from a
//! reference `Instant` shared with the probe. The orchestrator
//! captures one `Instant` and passes a clone to both crates; this
//! function receives it as a parameter.

use std::time::Instant;

use is_core::{ResourceSample, ResourceTimeline};
use tokio::sync::oneshot;
use tokio::time::{interval, MissedTickBehavior};

use crate::config::SysmonConfig;
use crate::error::SysmonError;
use crate::parse::{parse_stat, parse_status};

/// Reads /proc once for the given PID and returns one sample.
///
/// The `start` instant defines the origin of `elapsed_ns`. The
/// function records the moment of sampling immediately before the
/// reads, so `elapsed_ns` is a faithful timestamp of when the
/// snapshot was taken rather than when the I/O completed.
pub async fn sample_once(pid: u32, start: Instant) -> Result<ResourceSample, SysmonError> {
    let elapsed_ns = start.elapsed().as_nanos() as u64;

    let status_path = format!("/proc/{pid}/status");
    let stat_path = format!("/proc/{pid}/stat");

    let status_content =
        tokio::fs::read_to_string(&status_path)
            .await
            .map_err(|e| SysmonError::Io {
                path: status_path.clone(),
                source: e,
            })?;
    let stat_content =
        tokio::fs::read_to_string(&stat_path)
            .await
            .map_err(|e| SysmonError::Io {
                path: stat_path.clone(),
                source: e,
            })?;

    let status = parse_status(&status_content, &status_path)?;
    let stat = parse_stat(&stat_content, &stat_path)?;

    Ok(ResourceSample {
        elapsed_ns,
        rss_bytes: status.rss_bytes,
        cpu_user_jiffies: stat.cpu_user_jiffies,
        cpu_system_jiffies: stat.cpu_system_jiffies,
        thread_count: status.thread_count,
    })
}


/// Reads /proc once for the given PID and its direct children,
/// returning a single aggregated sample.
///
/// The parent's `elapsed_ns` is preserved as the timestamp (it is
/// the PID the user asked about). The numeric resource fields are
/// summed across parent and every successfully-sampled child:
///
/// - `rss_bytes`: total resident memory held by the process group
/// - `cpu_user_jiffies` / `cpu_system_jiffies`: total CPU time
/// - `thread_count`: total live threads
///
/// Saturating arithmetic guards the `u32` thread count against
/// overflow on pathological inputs; `u64` fields cannot
/// realistically overflow but use the same operator for uniformity.
///
/// Failure modes:
///
/// - Reading `/proc/<pid>/status` or `/proc/<pid>/stat` fails —
///   the function propagates the error. The parent PID being
///   unreadable means the sample is meaningless and the caller
///   should know.
/// - Reading `/proc/<pid>/task/<pid>/children` fails — the
///   function returns the parent-only sample. The kernel may not
///   expose children for very short-lived processes or unusual
///   namespaces; in either case the parent sample is still useful.
/// - Reading a specific child fails — silently skipped. A child
///   can exit between discovery and sample (race), and a
///   permission error on one child should not poison the whole
///   sample. See ADR-006.
pub async fn sample_once_aggregated(
    pid: u32,
    start: Instant,
) -> Result<ResourceSample, SysmonError> {
    let parent = sample_once(pid, start).await?;

    let children_path = format!("/proc/{pid}/task/{pid}/children");
    let children_content = match tokio::fs::read_to_string(&children_path).await {
        Ok(c) => c,
        Err(_) => return Ok(parent),
    };
    let child_pids = crate::parse::parse_children(&children_content, &children_path)
        .unwrap_or_default();

    if child_pids.is_empty() {
        return Ok(parent);
    }

    let mut agg = parent;
    for child_pid in child_pids {
        if let Ok(child_sample) = sample_once(child_pid, start).await {
            agg.rss_bytes = agg.rss_bytes.saturating_add(child_sample.rss_bytes);
            agg.cpu_user_jiffies = agg
                .cpu_user_jiffies
                .saturating_add(child_sample.cpu_user_jiffies);
            agg.cpu_system_jiffies = agg
                .cpu_system_jiffies
                .saturating_add(child_sample.cpu_system_jiffies);
            agg.thread_count = agg.thread_count.saturating_add(child_sample.thread_count);
        }
    }

    Ok(agg)
}
/// Runs the sampling loop until cancelled.
///
/// Samples `config.pid` every `config.sample_period`, accumulating
/// each successful sample into a [`ResourceTimeline`]. The loop
/// terminates when `cancel` is signalled (the orchestrator drops
/// the [`oneshot::Sender`], or sends a unit value through it).
/// Whatever samples have been collected up to that point are
/// returned in the timeline.
///
/// Sampling errors are tolerated by default: if a single tick
/// fails (typically because the process exited between ticks),
/// the failure is logged via the optional `on_error` callback —
/// or silently swallowed if no callback is provided — and the
/// loop continues. This matches the profiler's "best effort"
/// contract: a probe that completes with an incomplete timeline
/// is more useful than one that aborts.
///
/// `MissedTickBehavior::Skip` is used so that if the loop falls
/// behind (e.g. a heavily loaded system), it does not try to
/// catch up with a burst of back-to-back samples.
pub async fn sample_during(
    config: SysmonConfig,
    start: Instant,
    mut cancel: oneshot::Receiver<()>,
) -> ResourceTimeline {
    let mut timeline = ResourceTimeline::new(config.sample_period.as_nanos() as u64);

    let mut ticker = interval(config.sample_period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            // Cancellation wins over the tick: if both are ready
            // we stop without taking one more sample. `biased`
            // gives select! a deterministic poll order.
            _ = &mut cancel => break,

            _ = ticker.tick() => {
                match sample_once(config.pid, start).await {
                    Ok(sample) => timeline.push(sample),
                    Err(_) => {
                        // Best-effort: swallow per-tick errors so
                        // a transient failure (process dying, a
                        // brief /proc glitch) does not abort the
                        // whole timeline. A future revision can
                        // expose these via tracing or a callback.
                    }
                }
            }
        }
    }

    timeline
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sample_once_aggregated_falls_back_when_no_children() {
        // The test process itself has no forked children (cargo
        // test does not fork). Aggregating on our own PID must
        // therefore succeed and return values matching what
        // sample_once would have produced for the parent alone.
        let pid = std::process::id();
        let start = Instant::now();

        let single = sample_once(pid, start).await.unwrap();
        let aggregated = sample_once_aggregated(pid, start).await.unwrap();

        // RSS / threads can drift by a tiny amount between the two
        // reads (allocator activity, scheduler), so we assert the
        // *shape* — both calls succeeded, the aggregated sample is
        // not absurdly different from the single one. Equality is
        // not what we are testing here.
        assert_eq!(aggregated.elapsed_ns >= single.elapsed_ns, true);
        // Allow up to 50% drift on RSS — generous, but enough to
        // catch the failure mode where children are double-counted
        // against an empty children file.
        let rss_ratio = aggregated.rss_bytes as f64 / single.rss_bytes.max(1) as f64;
        assert!(
            (0.5..=1.5).contains(&rss_ratio),
            "aggregated RSS {} drifted too far from single RSS {} (ratio {:.2})",
            aggregated.rss_bytes,
            single.rss_bytes,
            rss_ratio
        );
    }

    #[tokio::test]
    async fn sample_once_aggregated_propagates_parent_read_failure() {
        // u32::MAX is guaranteed not to be a live PID. The parent
        // read must fail, and the error must propagate (not be
        // swallowed by the children-fallback path).
        let start = Instant::now();
        let err = sample_once_aggregated(u32::MAX, start).await.unwrap_err();
        assert!(
            matches!(err, SysmonError::Io { .. }),
            "expected Io error for nonexistent PID, got {err:?}"
        );
    }
}
