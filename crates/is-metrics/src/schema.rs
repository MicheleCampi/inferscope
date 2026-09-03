//! The metric vocabulary each engine speaks (ADR-014 D1).
//!
//! The series this crate reads are not the same names on every engine,
//! and the mapping is not a rename table: on vLLM the hit-rate
//! denominator and the prefill token count are two distinct series
//! (`vllm:prefix_cache_queries`, `vllm:prompt_tokens_total`), while on
//! SGLang both roles collapse onto `sglang:prompt_tokens_total`. A
//! configurable name-per-role map cannot express that collision without
//! letting a caller declare an incoherent pair. [`EngineSchema`] is a
//! compile-time type instead: the names are constants, and the schema
//! selects among them.
//!
//! The type stays `pub(crate)`. It is the vocabulary of a scrape, not a
//! fact about the measurement — what leaves this crate is values and
//! provenance, never the series names that produced them.
//!
//! `model_name` is deliberately not part of the schema: both engines
//! label their series with that same key (vLLM natively; SGLang from the
//! tokenizer collector's label dict, `served_model_name` under the
//! `model_name` key), so it is run-level configuration, not vocabulary.

use crate::config::Engine;

/// How the lines of one metric family combine into a single value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aggregation {
    /// One line carries the value; the first match wins.
    Single,

    /// The family is split across `label`, and the total is the sum of
    /// the lines whose `label` value is not `excluded`.
    ///
    /// This exists for `sglang:cached_tokens_total`, which SGLang emits
    /// either per cache source (`device`, `host`, `storage_<backend>`)
    /// or, on the backward-compatible path, once under the reserved
    /// value `total`. The two paths are mutually exclusive per request
    /// but the counter is cumulative, so one body can carry both
    /// families and a blind sum would double-count. Excluding the
    /// reserved value is exact under either path; a whitelist of source
    /// names would silently drop a `storage_<backend>` this code has
    /// never seen.
    SumOverLabel {
        /// The label the family is split across.
        label: &'static str,
        /// The reserved label value that repeats the whole, and is
        /// therefore excluded from the sum.
        excluded: &'static str,
    },
}

/// One metric family: the series name and how its lines combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Series {
    /// The exact metric name, matched on the text before `{`.
    pub(crate) name: &'static str,
    /// How the matching lines reduce to one value.
    pub(crate) aggregation: Aggregation,
}

impl Series {
    /// A family carried by a single line.
    const fn single(name: &'static str) -> Self {
        Self {
            name,
            aggregation: Aggregation::Single,
        }
    }
}

/// The series one engine exposes for the signals this crate reads.
///
/// Absence is expressed in the type: a signal an engine does not expose
/// is `None`, which is a declared capability gap and not a failure to
/// find something that should have been there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EngineSchema {
    /// Cached prompt tokens: the hit-rate numerator.
    pub(crate) hit_numerator: Series,

    /// Queried prompt tokens: the hit-rate denominator. On SGLang this
    /// is the same series as [`Self::prompt_tokens`] — the collision
    /// that motivates a schema rather than a name map.
    pub(crate) hit_denominator: Series,

    /// Cumulative prompt (prefill) tokens.
    pub(crate) prompt_tokens: Series,

    /// Cumulative generation (decode) tokens.
    pub(crate) generation_tokens: Series,

    /// Total seconds spent in prefill, as a histogram `_sum` line, or
    /// `None` where the engine exposes no such counter (ADR-014 D3).
    pub(crate) prefill_time_sum: Option<&'static str>,

    /// Total seconds spent in decode, or `None` — see
    /// [`Self::prefill_time_sum`].
    pub(crate) decode_time_sum: Option<&'static str>,

    /// Draft tokens proposed by speculative decoding, or `None` where the
    /// engine does not speculate or does not count it.
    ///
    /// This is the work that was *done*: every draft token cost a forward
    /// pass whether or not it survived verification. Paired with
    /// [`Self::spec_accepted_tokens`] it gives the acceptance rate, and
    /// paired with the energy counters it gives what the rejected fraction
    /// cost in joules — which is the quantity latency benchmarks cannot see.
    pub(crate) spec_draft_tokens: Option<Series>,

    /// Draft tokens that survived verification, or `None`.
    pub(crate) spec_accepted_tokens: Option<Series>,

    /// Number of speculation rounds, or `None`. Divides the two counters
    /// above into a per-round mean acceptance length, which is the unit the
    /// engine's own tuning parameters are expressed in.
    pub(crate) spec_drafts: Option<Series>,
}

/// The vLLM vocabulary (ADR-011, ADR-012).
pub(crate) const VLLM_SCHEMA: EngineSchema = EngineSchema {
    // The `_total` suffix is what the endpoint exposes, not what vLLM
    // registers. `loggers.py` names these counters without it, but they
    // are `prometheus_client.Counter`, and that library appends `_total`
    // on exposition unless the name already carries it. The two token
    // counters below happen to be spelled with the suffix and so always
    // matched; these two were spelled as registered and never matched a
    // real vLLM endpoint (verified against prometheus_client 0.26.0).
    hit_numerator: Series::single("vllm:prefix_cache_hits_total"),
    hit_denominator: Series::single("vllm:prefix_cache_queries_total"),
    prompt_tokens: Series::single("vllm:prompt_tokens_total"),
    generation_tokens: Series::single("vllm:generation_tokens_total"),
    prefill_time_sum: Some("vllm:request_prefill_time_seconds_sum"),
    decode_time_sum: Some("vllm:request_decode_time_seconds_sum"),
    // Same `_total` exposition rule as the two counters above: these are
    // registered without the suffix and exposed with it.
    spec_draft_tokens: Some(Series::single("vllm:spec_decode_num_draft_tokens_total")),
    spec_accepted_tokens: Some(Series::single("vllm:spec_decode_num_accepted_tokens_total")),
    spec_drafts: Some(Series::single("vllm:spec_decode_num_drafts_total")),
};

/// The SGLang vocabulary, verified at source against the tokenizer
/// metrics collector (ADR-014).
///
/// Two properties are load-bearing and neither is guessable from the
/// vLLM shape. The numerator is summed over `cache_source` with the
/// reserved `total` excluded. The denominator is the prompt token
/// counter itself: SGLang exposes no separate queried-tokens series, so
/// the hit-rate denominator and the prefill token count are one series
/// serving two roles.
///
/// The timing legs are `None`: SGLang's per-phase seconds are not
/// exposed as phase-separated counters. `sglang:cache_hit_rate` is a
/// gauge and is not read at all — a `mostrecent` gauge has no meaning
/// under the window differencing this crate performs.
pub(crate) const SGLANG_SCHEMA: EngineSchema = EngineSchema {
    hit_numerator: Series {
        name: "sglang:cached_tokens_total",
        aggregation: Aggregation::SumOverLabel {
            label: "cache_source",
            excluded: "total",
        },
    },
    hit_denominator: Series::single("sglang:prompt_tokens_total"),
    prompt_tokens: Series::single("sglang:prompt_tokens_total"),
    generation_tokens: Series::single("sglang:generation_tokens_total"),
    prefill_time_sum: None,
    decode_time_sum: None,
    // SGLang exposes speculative decoding as gauges, not counters.
    // `sglang:spec_accept_length` and `sglang:spec_accept_rate` are means
    // over the current batch; `sglang:spec_num_steps` and
    // `sglang:spec_num_draft_tokens` report the active configuration rather
    // than work done; `sglang:spec_cap_length` and
    // `sglang:spec_block_accept_length` are per-verify-step means. All six
    // are `Gauge` with `multiprocess_mode="mostrecent"`, verified at source
    // in `observability/metrics_collector.py` at sgl-project/sglang
    // `54cadad`. A gauge carrying a current mean has no meaning under the
    // window differencing this crate performs - the same reason
    // `sglang:cache_hit_rate` is not read either.
    //
    // One cumulative speculative counter does exist,
    // `sglang:spec_verify_calls_total`, and it would difference cleanly.
    // It counts verification calls, which is neither a draft-token count
    // nor an accepted-token count, so it yields no acceptance rate and no
    // acceptance length on its own. The gap is therefore in what is
    // counted, not in whether anything is: SGLang has no cumulative
    // draft/accepted pair, and one counter that cannot substitute for it.
    spec_draft_tokens: None,
    spec_accepted_tokens: None,
    spec_drafts: None,
};

impl Engine {
    /// The vocabulary this engine speaks.
    ///
    /// `page_size` does not select the schema: it decides how the
    /// numerator is accounted (see [`Engine::accounting`]), not which
    /// series carries it.
    pub(crate) fn schema(self) -> &'static EngineSchema {
        match self {
            Engine::Vllm => &VLLM_SCHEMA,
            Engine::Sglang { .. } => &SGLANG_SCHEMA,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sglang_collides_the_denominator_onto_the_prompt_counter() {
        // The collision ADR-014 D1 is built around: one series, two
        // roles. If this ever stops holding, a name map would have
        // become sufficient and the schema could be reconsidered.
        assert_eq!(
            SGLANG_SCHEMA.hit_denominator.name,
            SGLANG_SCHEMA.prompt_tokens.name
        );
    }

    #[test]
    fn vllm_keeps_the_denominator_and_the_prompt_counter_apart() {
        assert_ne!(
            VLLM_SCHEMA.hit_denominator.name,
            VLLM_SCHEMA.prompt_tokens.name
        );
    }

    #[test]
    fn only_the_sglang_numerator_is_summed() {
        assert_eq!(VLLM_SCHEMA.hit_numerator.aggregation, Aggregation::Single);
        assert_eq!(
            SGLANG_SCHEMA.hit_numerator.aggregation,
            Aggregation::SumOverLabel {
                label: "cache_source",
                excluded: "total",
            }
        );
    }

    #[test]
    fn sglang_declares_no_phase_timing() {
        assert!(SGLANG_SCHEMA.prefill_time_sum.is_none());
        assert!(SGLANG_SCHEMA.decode_time_sum.is_none());
        assert!(VLLM_SCHEMA.prefill_time_sum.is_some());
        assert!(VLLM_SCHEMA.decode_time_sum.is_some());
    }

    #[test]
    fn page_size_does_not_select_the_schema() {
        for page_size in [1, 16, 64] {
            assert_eq!(Engine::Sglang { page_size }.schema(), &SGLANG_SCHEMA);
        }
        assert_eq!(Engine::Vllm.schema(), &VLLM_SCHEMA);
    }
}
