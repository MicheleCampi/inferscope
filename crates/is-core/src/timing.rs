//! Raw token timing data for a single inference request.
//!
//! These types hold the raw signal captured by the probe: when each
//! token arrived, measured as nanoseconds since the request was sent.
//! No derived metric (time-to-first-token, inter-token latency,
//! tokens-per-second) is computed here — see ADR-002. Derived metrics
//! are the responsibility of the reporting layer.

use serde::{Deserialize, Serialize};

/// The arrival of a single generated token.
///
/// `elapsed_ns` is the time between the moment the request was sent
/// and the moment this token was received, in nanoseconds. The probe
/// captures one reference instant at request start and records each
/// token's offset from it; that reference instant never leaves the
/// probe, so this type carries only a plain integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenArrival {
    /// Zero-based position of this token in the generated sequence.
    pub index: u32,

    /// Nanoseconds elapsed from request start to this token's arrival.
    pub elapsed_ns: u64,
}

impl TokenArrival {
    /// Creates a new token arrival record.
    pub fn new(index: u32, elapsed_ns: u64) -> Self {
        Self { index, elapsed_ns }
    }
}

/// The complete raw timing record for one inference request.
///
/// Holds every token's arrival in generation order, plus the total
/// wall duration the probe observed for the request. The token
/// sequence is the primary signal; `total_ns` is recorded separately
/// because a request can take measurable time after its final token
/// (for example, the stream's terminating message).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestTiming {
    /// Token arrivals in generation order. Empty if the request
    /// produced no tokens.
    pub tokens: Vec<TokenArrival>,

    /// Total nanoseconds from request start to the request fully
    /// completing, including any time after the last token.
    pub total_ns: u64,
}

impl RequestTiming {
    /// Creates a timing record from a sequence of token arrivals and
    /// a total duration.
    pub fn new(tokens: Vec<TokenArrival>, total_ns: u64) -> Self {
        Self { tokens, total_ns }
    }

    /// Returns the number of tokens recorded for this request.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Returns `true` if no tokens were recorded.
    ///
    /// An empty timing record is a meaningful outcome, not an error:
    /// it describes a request that completed without producing any
    /// tokens.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_arrival_holds_its_fields() {
        let t = TokenArrival::new(3, 458_000_000);
        assert_eq!(t.index, 3);
        assert_eq!(t.elapsed_ns, 458_000_000);
    }

    #[test]
    fn request_timing_reports_token_count() {
        let timing = RequestTiming::new(
            vec![
                TokenArrival::new(0, 100),
                TokenArrival::new(1, 200),
                TokenArrival::new(2, 300),
            ],
            350,
        );
        assert_eq!(timing.token_count(), 3);
        assert!(!timing.is_empty());
    }

    #[test]
    fn empty_request_timing_is_empty() {
        let timing = RequestTiming::new(Vec::new(), 0);
        assert_eq!(timing.token_count(), 0);
        assert!(timing.is_empty());
    }

    #[test]
    fn request_timing_survives_json_round_trip() {
        let original = RequestTiming::new(
            vec![
                TokenArrival::new(0, 412_000_000),
                TokenArrival::new(1, 458_000_000),
            ],
            470_000_000,
        );

        let json = serde_json::to_string(&original).expect("RequestTiming should serialize");
        let restored: RequestTiming =
            serde_json::from_str(&json).expect("RequestTiming should deserialize");

        assert_eq!(original, restored);
    }
}
