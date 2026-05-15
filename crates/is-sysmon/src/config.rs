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
}

impl SysmonConfig {
    /// The default sampling period from ADR-003.
    pub const DEFAULT_PERIOD: Duration = Duration::from_millis(50);

    /// Creates a config with the default sampling period for the
    /// given PID.
    pub fn new(pid: u32) -> Self {
        Self {
            pid,
            sample_period: Self::DEFAULT_PERIOD,
        }
    }

    /// Creates a config with an explicit sampling period.
    pub fn with_period(pid: u32, sample_period: Duration) -> Self {
        Self { pid, sample_period }
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
}
