//! Configuration for a sysmon sampling run.
//!
//! [`SysmonConfig`] describes which process to sample and how
//! often. It carries no logic — the sampling loop receives a
//! config and reads its values, the same separation of parameter
//! handling from execution used in `is-probe`.

use std::time::Duration;

/// The configuration for one sysmon sampling run.
#[derive(Debug, Clone, Copy)]
pub struct SysmonConfig {
    /// The PID of the engine process to sample.
    pub pid: u32,
    /// The interval between samples. ADR-003 records 50 ms as the
    /// default; that is the value [`SysmonConfig::new`] applies.
    pub sample_period: Duration,
    /// Whether each sample should aggregate the target PID with
    /// the resource usage of its direct children, as recorded by
    /// `/proc/<pid>/task/<pid>/children` (see ADR-006).
    ///
    /// Default `false` preserves v0.1.0 / v0.2.0 behaviour: only
    /// the PID passed in is sampled. Set to `true` when the user
    /// passes a wrapper PID whose real workload runs in a forked
    /// child (typical of `llama-server` and similar inference
    /// engines).
    pub include_descendants: bool,
}

impl SysmonConfig {
    /// The default sampling period from ADR-003.
    pub const DEFAULT_PERIOD: Duration = Duration::from_millis(50);

    /// Creates a config with the default sampling period for the
    /// given PID, with descendant aggregation disabled.
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            sample_period: Self::DEFAULT_PERIOD,
            include_descendants: false,
        }
    }

    /// Creates a config with an explicit sampling period and
    /// descendant aggregation disabled.
    pub fn with_period(pid: u32, sample_period: Duration) -> Self {
        Self {
            pid,
            sample_period,
            include_descendants: false,
        }
    }

    /// Returns a copy of this config with descendant aggregation
    /// enabled. Use this when the target PID is known to fork a
    /// worker that does the real work (e.g. `llama-server`).
    pub fn with_descendants(self) -> Self {
        Self {
            include_descendants: true,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_applies_the_default_period() {
        let cfg = SysmonConfig::new(1234);
        assert_eq!(cfg.pid, 1234);
        assert_eq!(cfg.sample_period, Duration::from_millis(50));
        assert_eq!(cfg.sample_period, SysmonConfig::DEFAULT_PERIOD);
    }

    #[test]
    fn with_period_overrides_the_default() {
        let cfg = SysmonConfig::with_period(5678, Duration::from_millis(20));
        assert_eq!(cfg.pid, 5678);
        assert_eq!(cfg.sample_period, Duration::from_millis(20));
    }

    #[test]
    fn new_disables_descendants_by_default() {
        let cfg = SysmonConfig::new(1234);
        assert!(!cfg.include_descendants);
    }

    #[test]
    fn with_period_disables_descendants_by_default() {
        let cfg = SysmonConfig::with_period(5678, Duration::from_millis(20));
        assert!(!cfg.include_descendants);
    }

    #[test]
    fn with_descendants_enables_aggregation() {
        let cfg = SysmonConfig::new(1234).with_descendants();
        assert!(cfg.include_descendants);
        assert_eq!(cfg.pid, 1234);
        assert_eq!(cfg.sample_period, SysmonConfig::DEFAULT_PERIOD);
    }

    #[test]
    fn with_descendants_preserves_explicit_period() {
        let cfg = SysmonConfig::with_period(5678, Duration::from_millis(20)).with_descendants();
        assert!(cfg.include_descendants);
        assert_eq!(cfg.sample_period, Duration::from_millis(20));
    }
}
