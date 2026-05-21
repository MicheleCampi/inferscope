//! Integration test for sample_once_aggregated against a real
//! parent-child process pair.
//!
//! Spawns a bash shell that itself forks a `sleep` child, then
//! samples the bash PID twice — once with sample_once (parent
//! only) and once with sample_once_aggregated (parent + direct
//! children). The aggregated sample must report strictly more
//! threads than the parent-only sample, and at least as much
//! RSS. This is the end-to-end exercise of the path that crosses
//! parse_children → sample_once_aggregated → ResourceSample
//! summation, which the unit tests cover in isolation but not as
//! a chain.

use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use is_sysmon::sampler::{sample_once, sample_once_aggregated};

#[tokio::test]
async fn aggregated_sample_includes_child_resources() {
    // Spawn `bash -c "sleep 30 & wait"`:
    // - bash becomes the parent (the PID we monitor)
    // - sleep becomes a direct child of bash
    // - `wait` keeps bash alive so the parent does not exit
    //   between spawn and sampling
    let mut child = Command::new("bash")
        .args(["-c", "sleep 30 & wait"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("bash must be available on the test host");

    let bash_pid = child.id();

    // Give the kernel a moment to record the forked sleep child
    // in /proc/<bash_pid>/task/<bash_pid>/children. 100 ms is
    // generous on any modern host.
    sleep(Duration::from_millis(100));

    let start = Instant::now();

    let parent_only = sample_once(bash_pid, start)
        .await
        .expect("parent-only sample on live bash PID must succeed");
    let aggregated = sample_once_aggregated(bash_pid, start)
        .await
        .expect("aggregated sample on live bash PID must succeed");

    // Cleanup before any assert can panic — we don't want a
    // failed test to leave a 30 s sleep dangling.
    let _ = child.kill();
    let _ = child.wait();

    // The bash process is single-threaded; the sleep child adds
    // exactly one more thread to the aggregated view. The
    // assertion is the load-bearing one: it can only hold if
    // sample_once_aggregated actually walks /proc/<pid>/task/
    // <pid>/children and sums the child's /proc/<child>/status.
    assert!(
        aggregated.thread_count > parent_only.thread_count,
        "aggregated thread_count ({}) must exceed parent-only ({}); \
         indicates sample_once_aggregated did not include the child",
        aggregated.thread_count,
        parent_only.thread_count
    );

    // RSS is summed across the group, so the aggregated value
    // must be at least the parent-only value. Using >= rather
    // than > because in pathological allocator states the
    // difference could shrink below 4 KiB rounding.
    assert!(
        aggregated.rss_bytes >= parent_only.rss_bytes,
        "aggregated RSS ({}) must be >= parent-only RSS ({})",
        aggregated.rss_bytes,
        parent_only.rss_bytes
    );

    // The timestamp is preserved from the parent's sample; both
    // calls used the same `start` instant, so the aggregated
    // timestamp must be >= the parent-only one (aggregated runs
    // after).
    assert!(aggregated.elapsed_ns >= parent_only.elapsed_ns);
}
