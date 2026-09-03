//! Speculative-decoding counters scraped from an engine (ADR-015).
//!
//! Speculative decoding is tuned on latency: acceptance rate and tokens
//! per second. Neither says what the rejected drafts cost. A draft token
//! that fails verification is a forward pass that produced nothing, and
//! the only place that shows up is the energy counter.
//!
//! This timeline exists to put the two on one clock. `elapsed_ns` is
//! measured from the same reference instant as the GPU sampler, so a
//! speculative sample and an energy sample are correlated by direct
//! numeric comparison — the same contract [`crate::PhaseTimeline`] has
//! (ADR-003).

use serde::{Deserialize, Serialize};

/// One scrape of the speculative-decoding counters.
///
/// The three counters are one capability, not three: an acceptance rate
/// needs both a numerator and a denominator, and a mean acceptance length
/// needs the round count as well. An engine either exposes the family or
/// it does not, so a sample carrying a subset is not constructed — see
/// [`SpecTimeline::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecSample {
    /// Nanoseconds from the reference instant to when this scrape was
    /// taken.
    pub elapsed_ns: u64,

    /// Cumulative draft tokens proposed. This is work that was done
    /// regardless of what verification decided about it.
    pub draft_tokens: u64,

    /// Cumulative draft tokens that survived verification. The gap
    /// against `draft_tokens` is the wasted fraction.
    pub accepted_tokens: u64,

    /// Cumulative speculation rounds. Divides `accepted_tokens` into the
    /// mean acceptance length the engine's own tuning knobs are
    /// expressed in.
    pub drafts: u64,
}

/// A complete timeline of speculative-decoding scrapes for one probe run.
///
/// Samples are kept in the order they were taken, one per scrape tick,
/// for the same reason the other raw timelines are: the endpoints give a
/// window total, while the series shows whether acceptance drifted during
/// the run — which is the difference between a mean and a measurement.
///
/// An engine that does not expose the family produces an empty timeline,
/// which is a declared capability gap and not a failed scrape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecTimeline {
    /// Nominal scrape period. Actual gaps vary; informational.
    pub sample_period_ns: u64,
    /// Samples in scrape order.
    pub samples: Vec<SpecSample>,
}

impl SpecTimeline {
    /// An empty timeline for a run scraping at `sample_period_ns`.
    pub fn new(sample_period_ns: u64) -> Self {
        Self {
            sample_period_ns,
            samples: Vec::new(),
        }
    }

    /// Records a scrape, or drops it if the family was incomplete.
    ///
    /// Returns whether the sample was kept. A partial read means the
    /// endpoint carried some of the family and not the rest — a scrape
    /// that landed mid-registration, or an engine whose schema declares
    /// the family while the build does not emit it. Keeping such a
    /// sample would put a zero where a measurement is missing, and every
    /// derived rate downstream would inherit it.
    pub fn push(
        &mut self,
        elapsed_ns: u64,
        draft_tokens: Option<u64>,
        accepted_tokens: Option<u64>,
        drafts: Option<u64>,
    ) -> bool {
        let (Some(draft_tokens), Some(accepted_tokens), Some(drafts)) =
            (draft_tokens, accepted_tokens, drafts)
        else {
            return false;
        };
        self.samples.push(SpecSample {
            elapsed_ns,
            draft_tokens,
            accepted_tokens,
            drafts,
        });
        true
    }

    /// True when no sample was kept.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_keeps_a_complete_family() {
        let mut t = SpecTimeline::new(250_000_000);
        assert!(t.push(1_000, Some(40), Some(12), Some(10)));
        assert_eq!(t.samples.len(), 1);
        assert_eq!(t.samples[0].draft_tokens, 40);
        assert_eq!(t.samples[0].accepted_tokens, 12);
        assert_eq!(t.samples[0].drafts, 10);
    }

    #[test]
    fn push_drops_a_partial_family() {
        // Each of the three missing on its own must drop the sample: a
        // zero standing in for an unread counter would make every derived
        // rate wrong in the direction of "speculation is free".
        let mut t = SpecTimeline::new(250_000_000);
        assert!(!t.push(1_000, None, Some(12), Some(10)));
        assert!(!t.push(2_000, Some(40), None, Some(10)));
        assert!(!t.push(3_000, Some(40), Some(12), None));
        assert!(t.is_empty());
    }

    #[test]
    fn an_engine_without_the_family_yields_an_empty_timeline() {
        // Not a failure: SGLang exposes speculative decoding as gauges,
        // which this crate cannot difference over a window.
        let t = SpecTimeline::new(250_000_000);
        assert!(t.is_empty());
        assert_eq!(t.sample_period_ns, 250_000_000);
    }
}
