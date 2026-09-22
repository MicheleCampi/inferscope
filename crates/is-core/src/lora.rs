//! Active LoRA adapters, recorded as provenance of a sampling window
//! (ADR-017).
//!
//! `vllm:lora_requests_info` carries its data in its labels, and its value
//! is a time, not a count. What a report keeps is therefore not a timeline
//! (ADR-017 D1) but the outcome of one read taken as the window closes
//! (D6), in a form whose absences can be told apart (D4 and the
//! postscript).

use serde::{Deserialize, Serialize};

/// One series of `vllm:lora_requests_info`, as the producer exposed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoraSeries {
    /// Adapter names from the `running_lora_adapters` label, in the order
    /// the producer wrote them.
    pub running_lora_adapters: Vec<String>,

    /// Adapter names from the `waiting_lora_adapters` label, in the order
    /// the producer wrote them.
    pub waiting_lora_adapters: Vec<String>,

    /// The `max_lora` label. vLLM's Rust frontend builds its label set from
    /// the two adapter lists alone, so on that producer this is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lora: Option<u32>,

    /// The series value. Every producer read for ADR-017 writes the current
    /// time when it sets a series, and the most recent series is the one
    /// with the largest value.
    pub value: f64,
}

/// What one read of `vllm:lora_requests_info` found.
///
/// Four outcomes, kept apart because each means something different. None
/// of them is a statement about whether LoRA is configured: vLLM's Rust
/// frontend exposes no series while no adapter is active, so an empty read
/// does not distinguish a server without LoRA from one with no adapter in
/// use (ADR-017 postscript).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoraObservation {
    /// The read succeeded and the body held no series of the family.
    NoSeries,

    /// One series carried a value strictly larger than every other.
    Latest {
        /// That series.
        series: LoraSeries,
    },

    /// Several series shared the largest value. None is chosen: a producer
    /// that keeps old series and stamps them to the second can leave the
    /// current state and a stale one indistinguishable.
    Tie {
        /// Every series that carried the largest value, in body order.
        candidates: Vec<LoraSeries>,
    },

    /// The read did not complete. Nothing about the adapters is known.
    ReadFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(running: &[&str], max_lora: Option<u32>, value: f64) -> LoraSeries {
        LoraSeries {
            running_lora_adapters: running.iter().map(|s| s.to_string()).collect(),
            waiting_lora_adapters: Vec::new(),
            max_lora,
            value,
        }
    }

    fn round_trip(obs: &LoraObservation) -> LoraObservation {
        let json = serde_json::to_string(obs).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn every_outcome_survives_a_round_trip() {
        let all = [
            LoraObservation::NoSeries,
            LoraObservation::Latest {
                series: series(&["a1"], Some(8), 1.790060021e9),
            },
            LoraObservation::Tie {
                candidates: vec![
                    series(&[], Some(8), 1.790059976e9),
                    series(&["a2"], Some(8), 1.790059976e9),
                ],
            },
            LoraObservation::ReadFailed,
        ];
        for obs in &all {
            assert_eq!(&round_trip(obs), obs);
        }
    }

    #[test]
    fn an_absent_max_lora_is_left_out_of_the_json() {
        let json = serde_json::to_string(&series(&["d"], None, 1.0)).unwrap();
        assert!(!json.contains("max_lora"), "{json}");
        let back: LoraSeries = serde_json::from_str(&json).unwrap();
        assert_eq!(back.max_lora, None);
    }
}
