//! Configuration for a metrics scraping run.
//!
//! [`MetricsConfig`] describes which endpoint to scrape, which model's
//! series to read, and how often. Like [`is_sysmon`]'s `SysmonConfig`
//! it carries no logic — the scrape loop receives a config and reads
//! its values.
//!
//! The scrape period is deliberately separate from the probe's
//! token-timing sample period (ADR-003's 50 ms). A `/metrics` scrape is
//! a network round-trip reading application counters that change
//! per-request, not a local `/proc` read, so it has its own, more
//! relaxed cadence. The window hit rate (ADR-011) depends only on the
//! first and last samples, not the cadence between them; the cadence
//! governs only how finely the cache-warming curve is captured.

use std::time::Duration;

/// The configuration for one metrics scraping run.
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// The full URL of the `/metrics` endpoint to scrape, e.g.
    /// `http://127.0.0.1:18000/metrics`.
    pub endpoint: String,

    /// The value of the `model_name` label to select. A vLLM-schema
    /// endpoint may expose the `vllm:prefix_cache_*` series for more
    /// than one model; this picks the one this run is about. Per
    /// ADR-011 the label lives here, in run-level config, not in the
    /// per-sample raw types.
    pub model_name: String,

    /// The interval between scrapes. [`MetricsConfig::DEFAULT_PERIOD`]
    /// is applied by [`MetricsConfig::new`].
    pub sample_period: Duration,
}

impl MetricsConfig {
    /// The default scrape period: 1 second. Chosen independently of
    /// the 50 ms token-timing period — see the module docs for why a
    /// `/metrics` scrape wants a slower cadence.
    pub const DEFAULT_PERIOD: Duration = Duration::from_millis(1000);

    /// Creates a config with the default scrape period for the given
    /// endpoint and model.
    pub fn new(endpoint: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            model_name: model_name.into(),
            sample_period: Self::DEFAULT_PERIOD,
        }
    }

    /// Creates a config with an explicit scrape period.
    pub fn with_period(
        endpoint: impl Into<String>,
        model_name: impl Into<String>,
        sample_period: Duration,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            model_name: model_name.into(),
            sample_period,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_applies_the_default_period() {
        let cfg = MetricsConfig::new("http://127.0.0.1:18000/metrics", "facebook/opt-125m");
        assert_eq!(cfg.endpoint, "http://127.0.0.1:18000/metrics");
        assert_eq!(cfg.model_name, "facebook/opt-125m");
        assert_eq!(cfg.sample_period, Duration::from_millis(1000));
        assert_eq!(cfg.sample_period, MetricsConfig::DEFAULT_PERIOD);
    }

    #[test]
    fn with_period_overrides_the_default() {
        let cfg = MetricsConfig::with_period(
            "http://127.0.0.1:18000/metrics",
            "facebook/opt-125m",
            Duration::from_millis(250),
        );
        assert_eq!(cfg.sample_period, Duration::from_millis(250));
        assert_eq!(cfg.model_name, "facebook/opt-125m");
    }

    #[test]
    fn accepts_string_and_str_for_endpoint_and_model() {
        // Into<String> lets callers pass either &str or owned String
        // without ceremony; both must compile and store identically.
        let from_str = MetricsConfig::new("http://h/metrics", "m");
        let from_owned = MetricsConfig::new(String::from("http://h/metrics"), String::from("m"));
        assert_eq!(from_str.endpoint, from_owned.endpoint);
        assert_eq!(from_str.model_name, from_owned.model_name);
    }
}
