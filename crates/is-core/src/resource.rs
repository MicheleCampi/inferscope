//! Raw resource usage samples for a monitored engine process.
//!
//! These types hold the CPU-side resource footprint of the engine
//! process while a probe run is in progress. As with timing data
//! (see [`crate::timing`]), values are kept in the raw form the
//! kernel reports — bytes for memory, jiffies for CPU time — and
//! conversion to presentation units happens downstream. Same
//! lossless-signal principle as ADR-002 and ADR-003.

use serde::{Deserialize, Serialize};

/// A single sample of the engine process's resource state at one
/// moment in time.
///
/// `elapsed_ns` is the nanoseconds since the reference instant
/// shared with the probe, so a sample can be correlated with a
/// token arrival by direct numeric comparison.
///
/// CPU time fields are stored as raw scheduler jiffies. Converting
/// to seconds requires the system's `_SC_CLK_TCK` (typically 100),
/// which the reporting layer applies — the data layer carries the
/// signal as the kernel produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceSample {
    /// Nanoseconds from the reference instant to when this sample
    /// was taken.
    pub elapsed_ns: u64,

    /// Resident set size in bytes — memory actually held in RAM
    /// by the process at this moment.
    pub rss_bytes: u64,

    /// User-mode CPU time accumulated by the process since its
    /// start, in scheduler jiffies (field 14 of /proc/<pid>/stat).
    pub cpu_user_jiffies: u64,

    /// Kernel-mode CPU time accumulated by the process since its
    /// start, in scheduler jiffies (field 15 of /proc/<pid>/stat).
    pub cpu_system_jiffies: u64,

    /// Number of threads in the process at this moment.
    pub thread_count: u32,
}

/// A complete timeline of resource samples for one probe run.
///
/// Samples are kept in order of `elapsed_ns`. A consumer that wants
/// to know the process state at a specific point in time — for
/// example, the moment a token arrived — performs a binary search
/// or linear scan over `samples`.
///
/// `sample_period_ns` records the nominal sampling period the
/// sysmon used. Actual gaps between samples vary because of
/// scheduler jitter; the field is informational, not authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceTimeline {
    /// The samples, in order of `elapsed_ns`.
    pub samples: Vec<ResourceSample>,

    /// The nominal sampling period the sysmon was configured with,
    /// in nanoseconds.
    pub sample_period_ns: u64,
}

impl ResourceTimeline {
    /// Creates an empty timeline with the given nominal period.
    pub fn new(sample_period_ns: u64) -> Self {
        Self {
            samples: Vec::new(),
            sample_period_ns,
        }
    }

    /// Returns the number of samples in the timeline.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Returns `true` if no samples were taken.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Appends a sample. Caller is responsible for monotonicity:
    /// in normal use samples are pushed in `elapsed_ns` order.
    pub fn push(&mut self, sample: ResourceSample) {
        self.samples.push(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(elapsed_ns: u64, rss: u64) -> ResourceSample {
        ResourceSample {
            elapsed_ns,
            rss_bytes: rss,
            cpu_user_jiffies: 0,
            cpu_system_jiffies: 0,
            thread_count: 1,
        }
    }

    #[test]
    fn timeline_starts_empty() {
        let t = ResourceTimeline::new(50_000_000);
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert_eq!(t.sample_period_ns, 50_000_000);
    }

    #[test]
    fn push_appends_in_order() {
        let mut t = ResourceTimeline::new(50_000_000);
        t.push(sample(50_000_000, 100));
        t.push(sample(100_000_000, 200));
        t.push(sample(150_000_000, 300));
        assert_eq!(t.len(), 3);
        assert_eq!(t.samples[0].rss_bytes, 100);
        assert_eq!(t.samples[2].rss_bytes, 300);
    }

    #[test]
    fn resource_sample_survives_json_round_trip() {
        let original = ResourceSample {
            elapsed_ns: 412_000_000,
            rss_bytes: 612 * 1024 * 1024,
            cpu_user_jiffies: 1234,
            cpu_system_jiffies: 56,
            thread_count: 8,
        };

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: ResourceSample = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn resource_timeline_survives_json_round_trip() {
        let mut original = ResourceTimeline::new(50_000_000);
        original.push(sample(50_000_000, 100));
        original.push(sample(100_000_000, 200));

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: ResourceTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }
}
