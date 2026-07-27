//! Configuration for a metrics scraping run.
//!
//! [`MetricsConfig`] describes which endpoint to scrape, which engine
//! speaks there, which model's series to read, and how often. Like
//! [`is_sysmon`]'s `SysmonConfig` it carries no logic — the scrape loop
//! receives a config and reads its values.
//!
//! The scrape period is deliberately separate from the probe's
//! token-timing sample period (ADR-003's 50 ms). A `/metrics` scrape is
//! a network round-trip reading application counters that change
//! per-request, not a local `/proc` read, so it has its own, more
//! relaxed cadence. The window hit rate (ADR-011) depends only on the
//! first and last samples, not the cadence between them; the cadence
//! governs only how finely the cache-warming curve is captured.

use is_core::HitRateAccounting;
use std::time::Duration;

/// Which inference engine speaks on the scraped endpoint.
///
/// Selection is explicit and carries no default. A body matching neither
/// vocabulary would yield no series and, under any tolerant parse, zeros —
/// absence written as zero. The engine is declared by the caller
/// (ADR-014 D6), never sniffed from the response body.
///
/// `page_size` is carried by the SGLang variant alone because it is
/// load-bearing there alone: it decides whether the hit-rate numerator is
/// exact tokens or page-aligned. On vLLM the block size changes the
/// magnitude of the quantization bias (ADR-014 D5) but not the class of
/// accounting, which is block-aligned at every block size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// A vLLM-schema endpoint, exposing the `vllm:` metric vocabulary.
    Vllm,

    /// An SGLang endpoint, exposing the `sglang:` metric vocabulary.
    Sglang {
        /// The server's `page_size` as configured at engine start. It is
        /// not exposed on `/metrics`, so it cannot be derived from a
        /// scrape body and must be declared by the caller.
        page_size: u32,
    },
}

impl Engine {
    /// How this engine accounts the hit-rate numerator (ADR-014 D2).
    ///
    /// vLLM truncates the numerator to a block boundary at every block
    /// size. SGLang counts exact tokens only when the server runs at
    /// `page_size = 1`, and is page-aligned above it.
    pub(crate) fn accounting(self) -> HitRateAccounting {
        match self {
            Engine::Vllm => HitRateAccounting::BlockAligned,
            Engine::Sglang { page_size: 1 } => HitRateAccounting::ExactTokens,
            Engine::Sglang { .. } => HitRateAccounting::BlockAligned,
        }
    }
}

/// The configuration for one metrics scraping run.
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// The full URL of the `/metrics` endpoint to scrape, e.g.
    /// `http://127.0.0.1:18000/metrics`.
    pub endpoint: String,

    /// The value of the `model_name` label to select. An endpoint may
    /// expose the prefix-cache series for more than one model; this
    /// picks the one this run is about. Per ADR-011 the label lives
    /// here, in run-level config, not in the per-sample raw types.
    pub model_name: String,

    /// Which engine speaks on `endpoint`. Mandatory and without a
    /// default, per ADR-014 D6.
    pub engine: Engine,

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
    /// endpoint, model and engine.
    pub fn new(endpoint: impl Into<String>, model_name: impl Into<String>, engine: Engine) -> Self {
        Self {
            endpoint: endpoint.into(),
            model_name: model_name.into(),
            engine,
            sample_period: Self::DEFAULT_PERIOD,
        }
    }

    /// Creates a config with an explicit scrape period.
    pub fn with_period(
        endpoint: impl Into<String>,
        model_name: impl Into<String>,
        engine: Engine,
        sample_period: Duration,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            model_name: model_name.into(),
            engine,
            sample_period,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_applies_the_default_period() {
        let cfg = MetricsConfig::new(
            "http://127.0.0.1:18000/metrics",
            "facebook/opt-125m",
            Engine::Vllm,
        );
        assert_eq!(cfg.endpoint, "http://127.0.0.1:18000/metrics");
        assert_eq!(cfg.model_name, "facebook/opt-125m");
        assert_eq!(cfg.engine, Engine::Vllm);
        assert_eq!(cfg.sample_period, Duration::from_millis(1000));
        assert_eq!(cfg.sample_period, MetricsConfig::DEFAULT_PERIOD);
    }

    #[test]
    fn with_period_overrides_the_default() {
        let cfg = MetricsConfig::with_period(
            "http://127.0.0.1:18000/metrics",
            "facebook/opt-125m",
            Engine::Sglang { page_size: 1 },
            Duration::from_millis(250),
        );
        assert_eq!(cfg.sample_period, Duration::from_millis(250));
        assert_eq!(cfg.model_name, "facebook/opt-125m");
        assert_eq!(cfg.engine, Engine::Sglang { page_size: 1 });
    }

    #[test]
    fn accepts_string_and_str_for_endpoint_and_model() {
        // Into<String> lets callers pass either &str or owned String
        // without ceremony; both must compile and store identically.
        let from_str = MetricsConfig::new("http://h/metrics", "m", Engine::Vllm);
        let from_owned = MetricsConfig::new(
            String::from("http://h/metrics"),
            String::from("m"),
            Engine::Vllm,
        );
        assert_eq!(from_str.endpoint, from_owned.endpoint);
        assert_eq!(from_str.model_name, from_owned.model_name);
    }

    #[test]
    fn vllm_is_block_aligned_at_every_block_size() {
        // The block size is not a parameter of the variant precisely
        // because it does not change the class of accounting.
        assert_eq!(Engine::Vllm.accounting(), HitRateAccounting::BlockAligned);
    }

    #[test]
    fn sglang_counts_exact_tokens_only_at_page_size_one() {
        assert_eq!(
            Engine::Sglang { page_size: 1 }.accounting(),
            HitRateAccounting::ExactTokens
        );
    }

    #[test]
    fn sglang_above_page_size_one_is_page_aligned() {
        for page_size in [2, 16, 64] {
            assert_eq!(
                Engine::Sglang { page_size }.accounting(),
                HitRateAccounting::BlockAligned,
                "page_size {page_size} must not be read as exact tokens"
            );
        }
    }
}
