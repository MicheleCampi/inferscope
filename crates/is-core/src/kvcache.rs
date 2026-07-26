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
}

impl KvCacheTimeline {
    /// Creates an empty timeline with the given nominal period.
    pub fn new(sample_period_ns: u64) -> Self {
        Self {
            samples: Vec::new(),
            sample_period_ns,
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
        let t = KvCacheTimeline::new(500_000_000);
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert_eq!(t.sample_period_ns, 500_000_000);
    }

    #[test]
    fn push_appends_samples() {
        let mut t = KvCacheTimeline::new(500_000_000);
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
        let mut original = KvCacheTimeline::new(500_000_000);
        original.push(sample(500_000_000, 10, 40));
        original.push(sample(1_000_000_000, 48, 120));
        original.push(sample(1_500_000_000, 96, 196));

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: KvCacheTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }
}
