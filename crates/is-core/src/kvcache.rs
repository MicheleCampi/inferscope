//! KV-cache hit-rate samples scraped from an engine's Prometheus
//! `/metrics` endpoint.
//!
//! These types hold the prefix-cache counters a vLLM-schema engine
//! exposes — `vllm:prefix_cache_hits` and `vllm:prefix_cache_queries`,
//! both monotonic counters — sampled over the network while a probe run
//! is in progress (see ADR-011). They are inferscope's first
//! application-internal metric source: unlike `/proc` or NVML, the hit
//! rate cannot be observed from outside the engine process.
//!
//! Per ADR-005's discipline, the counters are stored as raw `u64` values
//! in their native form; the derived hit rate is a float computed at the
//! reporting layer ([`is-report`]), not here. Per ADR-003 each sample
//! carries `elapsed_ns` from the same reference instant the probe and the
//! other samplers use, so a KV-cache sample correlates with a token
//! arrival, a CPU sample, or a GPU sample by direct numeric comparison.

use serde::{Deserialize, Serialize};

/// How the engine accounts the hit-rate numerator.
///
/// The two engines inferscope reads do not count the same thing under
/// the same name. vLLM's `vllm:prefix_cache_hits` is truncated to a
/// block boundary by the engine while its denominator
/// (`vllm:prefix_cache_queries`) is exact tokens, so the rate
/// underestimates by at most one block per request that hit. SGLang's
/// `sglang:cached_tokens_total` is exact tokens when the server runs at
/// `page_size = 1`, and block-aligned above it.
///
/// `page_size` is server configuration and is not exposed on the
/// `/metrics` endpoint, so this cannot be derived from a scrape body:
/// it is declared by the caller when the schema is built, never
/// inferred (ADR-014 D2, D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitRateAccounting {
    /// The numerator is truncated to a block or page boundary; the
    /// rate is a lower bound on the exact-token rate.
    BlockAligned,

    /// Numerator and denominator are both exact token counts.
    ExactTokens,
}

/// A single scrape of the prefix-cache counters at one moment in time.
///
/// `hits` and `queries` are the raw cumulative counter values as read
/// from the endpoint — not deltas. The window delta and the derived hit
/// rate are computed at the reporting layer from the first and last
/// samples of a [`KvCacheTimeline`] (see ADR-011).
///
/// `elapsed_ns` is nanoseconds since the same reference instant the CPU
/// and GPU samplers use, so a KV-cache sample can be correlated with a
/// token arrival by direct numeric comparison (see ADR-003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvCacheSample {
    /// Nanoseconds from the reference instant to when this scrape was
    /// taken.
    pub elapsed_ns: u64,

    /// Cumulative value of `vllm:prefix_cache_hits` at scrape time:
    /// prefill tokens served from cache since the engine started.
    /// Counted in tokens, but truncated to a block boundary by the
    /// engine, so it underestimates by at most one block per request
    /// that hit (ADR-014).
    pub hits: u64,

    /// Cumulative value of `vllm:prefix_cache_queries` at scrape time:
    /// prefill tokens looked up in the cache since the engine
    /// started. Exact tokens, not blocks. The denominator of the
    /// hit rate.
    pub queries: u64,
}

/// A complete timeline of KV-cache scrapes for one probe run.
///
/// Samples are kept in the order they were taken, one per scrape tick.
/// Keeping the full series rather than just the endpoints (see ADR-011)
/// preserves the cache-warming curve — how the hit rate climbs as the
/// prefix cache fills — and keeps the schema uniform with the other raw
/// timelines ([`crate::GpuTimeline`], [`crate::ResourceTimeline`]).
///
/// `sample_period_ns` records the nominal scrape period. Actual gaps
/// vary; the field is informational.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KvCacheTimeline {
    /// The scrapes, in order of insertion.
    pub samples: Vec<KvCacheSample>,

    /// The nominal scrape period the metric source was configured with,
    /// in nanoseconds.
    pub sample_period_ns: u64,

    /// How the engine accounts the hit-rate numerator, declared when
    /// the metric source was built (ADR-014 D2).
    ///
    /// `None` in reports written before ADR-014. Readers resolve that
    /// case from the report's schema version rather than defaulting
    /// here, so absence is never silently read as a measurement
    /// (ADR-014 D7).
    #[serde(default)]
    pub accounting: Option<HitRateAccounting>,
}

impl KvCacheTimeline {
    /// Creates an empty timeline with the given nominal period and
    /// hit-rate accounting.
    ///
    /// The accounting is required here rather than optional: a timeline
    /// built by this crate always knows which engine produced it. The
    /// `None` case exists only for reports deserialized from before
    /// ADR-014.
    pub fn new(sample_period_ns: u64, accounting: HitRateAccounting) -> Self {
        Self {
            samples: Vec::new(),
            sample_period_ns,
            accounting: Some(accounting),
        }
    }

    /// Returns the number of scrapes in the timeline.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Returns `true` if no scrapes were taken.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Appends a scrape. Caller is responsible for sample order: in
    /// normal use samples are pushed in `elapsed_ns` order.
    pub fn push(&mut self, sample: KvCacheSample) {
        self.samples.push(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(elapsed_ns: u64, hits: u64, queries: u64) -> KvCacheSample {
        KvCacheSample {
            elapsed_ns,
            hits,
            queries,
        }
    }

    #[test]
    fn timeline_starts_empty() {
        let t = KvCacheTimeline::new(500_000_000, HitRateAccounting::BlockAligned);
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert_eq!(t.sample_period_ns, 500_000_000);
    }

    #[test]
    fn push_appends_samples() {
        let mut t = KvCacheTimeline::new(500_000_000, HitRateAccounting::BlockAligned);
        t.push(sample(500_000_000, 10, 40));
        t.push(sample(1_000_000_000, 48, 120));
        t.push(sample(1_500_000_000, 96, 196));
        assert_eq!(t.len(), 3);
        assert_eq!(t.samples[0].hits, 10);
        assert_eq!(t.samples[2].hits, 96);
        assert_eq!(t.samples[2].queries, 196);
    }

    #[test]
    fn kvcache_sample_survives_json_round_trip() {
        // Values from the Blocco A live measurement: hits=96,
        // queries=196 against facebook/opt-125m on the v0.8.2 sim.
        let original = sample(1_500_000_000, 96, 196);
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: KvCacheSample = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn kvcache_timeline_survives_json_round_trip() {
        let mut original = KvCacheTimeline::new(500_000_000, HitRateAccounting::BlockAligned);
        original.push(sample(500_000_000, 10, 40));
        original.push(sample(1_000_000_000, 48, 120));
        original.push(sample(1_500_000_000, 96, 196));

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: KvCacheTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn timeline_carries_the_accounting_it_was_built_with() {
        let t = KvCacheTimeline::new(500_000_000, HitRateAccounting::ExactTokens);
        assert_eq!(t.accounting, Some(HitRateAccounting::ExactTokens));

        let json = serde_json::to_string(&t).expect("serialize");
        let restored: KvCacheTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.accounting, Some(HitRateAccounting::ExactTokens));
        assert!(json.contains("exact_tokens"), "json was {json}");
    }

    #[test]
    fn a_report_written_before_adr_014_reads_as_unknown_accounting() {
        // The shape KvCacheTimeline serialized to before the field
        // existed. It must still parse, and the missing field must not
        // be resolved to a value here: absence is not a measurement
        // (ADR-014 D7).
        let legacy = r#"{"samples":[{"elapsed_ns":1,"hits":96,"queries":196}],
                         "sample_period_ns":500000000}"#;
        let restored: KvCacheTimeline = serde_json::from_str(legacy).expect("deserialize");
        assert_eq!(restored.accounting, None);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.samples[0].hits, 96);
    }
}
