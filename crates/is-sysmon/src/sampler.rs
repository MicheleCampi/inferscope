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
