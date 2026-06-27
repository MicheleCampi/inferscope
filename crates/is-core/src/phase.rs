//! Per-phase samples derived from an engine's Prometheus `/metrics`
//! endpoint: the prefill/decode token counts and the cumulative
//! time-in-phase, sampled over the network while a probe run is in
//! progress (see ADR-012).
//!
//! Like [`crate::KvCacheSample`] (ADR-011), these are an
//! application-internal metric source: the prefill/decode split is not
//! visible from `/proc` or NVML, only from the engine's own counters.
//! The phase token totals (`vllm:prompt_tokens_total`,
//! `vllm:generation_tokens_total`) and the phase time `_sum` series
//! (`vllm:request_prefill_time_seconds_sum`,
//! `vllm:request_decode_time_seconds_sum`) are scraped together.
//!
//! Per ADR-005's discipline the raw layer is integer-only: token totals
//! are stored as their native `u64` counters, and the phase times — which
//! arrive as float seconds on the wire — are converted to integer
//! nanoseconds at parse time, the same unit `elapsed_ns` uses. No `f64`
//! lives here; the apportionment ratios and the divergence are derived
//! floats computed at the reporting layer ([`is-report`]), not here.
//!
//! Per ADR-003 each sample carries `elapsed_ns` from the same reference
//! instant the probe and the other samplers use, so a phase sample
//! correlates with a GPU energy sample, a token arrival, or a CPU sample
//! by direct numeric comparison — which is what lets device energy be
//! apportioned across phases on one shared clock.
use serde::{Deserialize, Serialize};

/// A single scrape of the phase signals at one moment in time.
///
/// `prompt_tokens` and `generation_tokens` are the raw cumulative counter
/// values as read from the endpoint — not deltas. `prefill_ns` and
/// `decode_ns` are the cumulative time-in-phase, converted from the
/// histogram `_sum` float seconds to integer nanoseconds at parse time.
/// All four are cumulative since the engine started; the window deltas and
/// the derived apportionments are computed at the reporting layer from the
/// first and last samples of a [`PhaseTimeline`] (see ADR-012).
///
/// `elapsed_ns` is nanoseconds since the same reference instant the CPU
/// and GPU samplers use, so a phase sample can be correlated with a GPU
/// energy sample by direct numeric comparison (see ADR-003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseSample {
    /// Nanoseconds from the reference instant to when this scrape was
    /// taken.
    pub elapsed_ns: u64,
    /// Cumulative value of `vllm:prompt_tokens_total` at scrape time:
    /// prefill (prompt) tokens processed since the engine started.
    pub prompt_tokens: u64,
    /// Cumulative value of `vllm:generation_tokens_total` at scrape time:
    /// decode (generation) tokens produced since the engine started.
    pub generation_tokens: u64,
    /// Cumulative time spent in prefill, in nanoseconds: the
    /// `vllm:request_prefill_time_seconds_sum` value converted from
    /// seconds at parse time.
    pub prefill_ns: u64,
    /// Cumulative time spent in decode, in nanoseconds: the
    /// `vllm:request_decode_time_seconds_sum` value converted from
    /// seconds at parse time.
    pub decode_ns: u64,
}

/// A complete timeline of phase scrapes for one probe run.
///
/// Samples are kept in the order they were taken, one per scrape tick.
/// Keeping the full series rather than just the endpoints (see ADR-012,
/// following ADR-011) preserves the phase-split curve — how the
/// prefill/decode balance shifts as a run proceeds — and keeps the schema
/// uniform with the other raw timelines ([`crate::KvCacheTimeline`],
/// [`crate::GpuTimeline`], [`crate::ResourceTimeline`]).
///
/// `sample_period_ns` records the nominal scrape period. Actual gaps
/// vary; the field is informational.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTimeline {
    /// The scrapes, in order of insertion.
    pub samples: Vec<PhaseSample>,
    /// The nominal scrape period the metric source was configured with,
    /// in nanoseconds.
    pub sample_period_ns: u64,
}

impl PhaseTimeline {
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
    pub fn push(&mut self, sample: PhaseSample) {
        self.samples.push(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(
        elapsed_ns: u64,
        prompt: u64,
        generation: u64,
        prefill_ns: u64,
        decode_ns: u64,
    ) -> PhaseSample {
        PhaseSample {
            elapsed_ns,
            prompt_tokens: prompt,
            generation_tokens: generation,
            prefill_ns,
            decode_ns,
        }
    }

    #[test]
    fn new_timeline_is_empty() {
        let t = PhaseTimeline::new(500_000_000);
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert_eq!(t.sample_period_ns, 500_000_000);
    }

    #[test]
    fn push_appends_in_order() {
        let mut t = PhaseTimeline::new(500_000_000);
        t.push(sample(100, 10, 2, 5, 8));
        t.push(sample(200, 20, 6, 9, 14));
        assert_eq!(t.len(), 2);
        assert_eq!(t.samples[0].elapsed_ns, 100);
        assert_eq!(t.samples[1].generation_tokens, 6);
    }

    #[test]
    fn sample_roundtrips_through_json() {
        let s = sample(412_000_000, 196, 38, 14493, 28432);
        let json = serde_json::to_string(&s).expect("serialize");
        let restored: PhaseSample = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, restored);
    }

    #[test]
    fn timeline_roundtrips_through_json() {
        let mut original = PhaseTimeline::new(500_000_000);
        original.push(sample(100, 10, 2, 5, 8));
        original.push(sample(200, 20, 6, 9, 14));
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: PhaseTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }
}
