//! Integration tests for the sysmon sampling loop.
//!
//! Unlike the parser tests in src/parse.rs, which work on synthetic
//! /proc content, these tests perform real I/O. They monitor a
//! known live process — the test binary itself — so the existence
//! of /proc/<pid>/{status,stat} is guaranteed.

use std::time::{Duration, Instant};

use is_sysmon::config::SysmonConfig;
use is_sysmon::sampler::{sample_during, sample_once};
use tokio::sync::oneshot;

/// The PID of the current test process. Always a live process,
/// so /proc/<self>/{status,stat} are guaranteed to be readable.
fn self_pid() -> u32 {
    std::process::id()
}

#[tokio::test]
async fn sample_once_reads_real_proc_for_self() {
    let start = Instant::now();
    let sample = sample_once(self_pid(), start)
        .await
        .expect("reading /proc for the test process must succeed");

    // Sanity: a live process holds some memory and has at least
    // one thread. We don't pin exact values, only that the
    // readings look plausible.
    assert!(
        sample.rss_bytes > 0,
        "RSS should be non-zero for a running process"
    );
    assert!(
        sample.thread_count >= 1,
        "thread count should be at least 1"
    );

    // elapsed_ns is measured from start; it should be a small but
    // strictly positive value (we just read a few hundred bytes).
    // Allow up to a generous upper bound for very slow CI.
    assert!(sample.elapsed_ns > 0);
    assert!(
        sample.elapsed_ns < 5_000_000_000,
        "sample took unreasonably long: {} ns",
        sample.elapsed_ns
    );
}

#[tokio::test]
async fn sample_once_errors_on_nonexistent_pid() {
    // PID 0 is reserved by the kernel and never corresponds to a
    // user process; /proc/0 does not exist.
    let start = Instant::now();
    let err = sample_once(0, start)
        .await
        .expect_err("sampling PID 0 must fail");
    assert!(
        matches!(err, is_sysmon::error::SysmonError::Io { .. }),
        "expected Io error, got {err:?}"
    );
}

#[tokio::test]
async fn sample_during_collects_multiple_samples_until_cancelled() {
    let cfg = SysmonConfig::with_period(self_pid(), Duration::from_millis(20));
    let start = Instant::now();
    let (tx, rx) = oneshot::channel();

    // Spawn the sampler. After ~100 ms — long enough for several
    // 20 ms ticks — cancel it and wait for the timeline.
    let handle = tokio::spawn(sample_during(cfg, start, rx));

    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = tx.send(());

    let timeline = handle.await.expect("sampler task should not panic");

    // At least 2 samples should have landed in 100 ms at 20 ms
    // cadence. We don't assert an exact count because the first
    // tick fires immediately and scheduling jitter affects the rest.
    assert!(
        timeline.len() >= 2,
        "expected at least 2 samples, got {}",
        timeline.len()
    );

    // elapsed_ns is non-decreasing across the timeline.
    for pair in timeline.samples.windows(2) {
        assert!(pair[1].elapsed_ns >= pair[0].elapsed_ns);
    }

    // The configured period travelled with the timeline.
    assert_eq!(timeline.sample_period_ns, 20_000_000);
}

#[tokio::test]
async fn sample_during_returns_an_empty_timeline_if_cancelled_immediately() {
    let cfg = SysmonConfig::with_period(self_pid(), Duration::from_millis(100));
    let start = Instant::now();
    let (tx, rx) = oneshot::channel();

    // Cancel before any tick can fire.
    let _ = tx.send(());
    let timeline = sample_during(cfg, start, rx).await;

    // The first tick of tokio's interval fires immediately, but
    // `biased` polling makes cancellation win when both are
    // ready. A cancellation observed before any work is therefore
    // expected to produce zero samples.
    assert!(
        timeline.is_empty(),
        "expected empty timeline, got {} samples",
        timeline.len()
    );
}
