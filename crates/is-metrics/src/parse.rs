//! Pure parsing of Prometheus text-exposition content.
//!
//! This module is I/O-free, mirroring [`is_sysmon`]'s `parse` module:
//! the scrape loop reads the HTTP body, hands the text here, and this
//! code turns it into counter values. No network, no async.
//!
//! Which series are read is not decided here. The caller declares the
//! engine and [`crate::schema::EngineSchema`] names the series and says
//! how their lines combine (ADR-014 D1); this module knows only the
//! shape of a text-exposition line. That split is what lets one parser
//! serve two vocabularies whose roles do not map one-to-one.
//!
//! Scope stays narrow (ADR-011): counter series and histogram `_sum`
//! lines, selected by the `model_name` label. Histogram `_bucket` and
//! `_count` lines are ignored — an exact match on the full metric name
//! excludes them. Extending to buckets would be parser work this module
//! does not yet do; that trade-off is recorded in ADR-011.

use crate::config::Engine;
use crate::error::MetricsError;
use crate::schema::{Aggregation, Series};

/// Extracts the value of one label from a Prometheus label block.
///
/// `labels` is the raw content between the braces, without the braces
/// themselves: e.g. `model_name="facebook/opt-125m",le="0.3"`. Returns
/// the value of `key` (the text inside the quotes) if present.
///
/// This handles the multi-label case: `model_name` may not be the only
/// label, nor the first. The search is for `key="` followed by the
/// value up to the closing quote, so label order does not matter. It
/// matters more on SGLang than on vLLM: the tokenizer collector labels
/// every series with `engine_type` and, depending on server flags, with
/// ranks and operator-supplied custom labels alongside `model_name`.
fn extract_label<'a>(labels: &'a str, key: &str) -> Option<&'a str> {
    // Build the needle `key="` and find it. Anchoring on the equals and
    // quote avoids matching a key that is a suffix of another (e.g.
    // searching "name" must not match "model_name").
    for part in labels.split(',') {
        let part = part.trim();
        let (k, v) = part.split_once('=')?;
        if k.trim() == key {
            // v is a quoted string: "value". Strip the surrounding
            // quotes. If it is not quoted as expected, skip.
            let v = v.trim();
            let unquoted = v.strip_prefix('"').and_then(|s| s.strip_suffix('"'))?;
            return Some(unquoted);
        }
    }
    None
}

/// Reads one line's value as a counter.
///
/// The value is read as `f64` then converted to `u64`: the Prometheus
/// wire format may render any value in float or scientific notation even
/// for a counter (the sim emits e.g. `0.001509284` for latency sums), so
/// reading as `f64` is robust where reading as `u64` directly would not
/// be. A counter is conceptually a non-negative integer; the conversion
/// truncates toward zero, which is exact for the integer values these
/// counters actually hold.
fn parse_counter_value(value: &str, metric: &str) -> Result<u64, MetricsError> {
    let parsed: f64 = value.parse().map_err(|_| MetricsError::Parse {
        detail: format!("metric {metric}: value {value:?} is not a number"),
    })?;
    if parsed < 0.0 || !parsed.is_finite() {
        return Err(MetricsError::Parse {
            detail: format!("metric {metric}: value {parsed} is not a valid counter"),
        });
    }
    Ok(parsed as u64)
}

/// Reads one series from a text-exposition body, as the schema declares it.
///
/// Selects the lines whose metric name is exactly `series.name` and whose
/// `model_name` label equals `model_name`, then reduces them per
/// `series.aggregation`: [`Aggregation::Single`] takes the first match,
/// [`Aggregation::SumOverLabel`] adds every match whose split label is not
/// the reserved excluded value, and [`Aggregation::SumOverAll`] adds every
/// match.
///
/// The distinction between `Single` and the two summing forms is not a
/// performance choice. `Single` returns on the first matching line, so on a
/// family that emits more than one it reads a part and reports it as the
/// whole, silently and in a direction that depends on exposition order.
///
/// `Ok(None)` means the series is not in the body — a fact, not a failure.
/// A hit-rate numerator can legitimately be absent (SGLang emits no
/// `cached_tokens_total` line until a request has hit the prefix cache, and
/// emits only the reserved `total` on its backward-compatible path), and
/// absence must not be readable as zero. Deciding whether an absent series
/// is tolerable belongs to the caller, which knows the role the series
/// plays. `Err` is reserved for a line that exists but does not parse.
///
/// Lines beginning with `#` (HELP, TYPE, and documentary comments) are
/// skipped. The metric name match is exact on the text before `{`, so
/// `prefix_cache_hits` never matches a longer name that contains it.
fn parse_series(
    body: &str,
    series: &Series,
    model_name: &str,
) -> Result<Option<u64>, MetricsError> {
    let mut summed: Option<u64> = None;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split into "name{labels}" and "value" on the last whitespace.
        let Some((raw, value)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let raw = raw.trim();
        let value = value.trim();

        // Separate the metric name from the label block.
        let (name, labels) = match raw.split_once('{') {
            Some((name, rest)) => {
                let labels = rest.strip_suffix('}').unwrap_or(rest);
                (name, labels)
            }
            // A series with no labels: name is the whole thing. Not the
            // shape our counters take, but handled rather than panicked.
            None => (raw, ""),
        };

        if name != series.name {
            continue;
        }
        if extract_label(labels, "model_name") != Some(model_name) {
            continue;
        }

        // The reserved value repeats the whole family and would be
        // double-counted. A line missing the split label entirely is kept:
        // only the named reserved value is excluded, never a source name
        // this code has not seen.
        if let Aggregation::SumOverLabel { label, excluded } = series.aggregation {
            if extract_label(labels, label) == Some(excluded) {
                continue;
            }
        }

        let parsed = parse_counter_value(value, series.name)?;

        match series.aggregation {
            Aggregation::Single => return Ok(Some(parsed)),
            Aggregation::SumOverLabel { .. } | Aggregation::SumOverAll { .. } => {
                let running = summed.unwrap_or(0);
                summed = Some(
                    running
                        .checked_add(parsed)
                        .ok_or_else(|| MetricsError::Parse {
                            detail: format!("metric {}: summed value overflows u64", series.name),
                        })?,
                );
            }
        }
    }

    Ok(summed)
}

/// Turns the absence of a series the caller requires into a `Parse` error.
fn require(value: Option<u64>, metric: &str, model_name: &str) -> Result<u64, MetricsError> {
    value.ok_or_else(|| MetricsError::Parse {
        detail: format!("metric {metric} not found for model_name {model_name:?}"),
    })
}

/// Parses one histogram `_sum` series as integer nanoseconds.
///
/// Phase timing in the vLLM schema is exposed as histogram families
/// (`request_prefill_time_seconds`, `request_decode_time_seconds`); the
/// `_sum` line of each is a single `metric{labels} value` line — the same
/// shape a counter takes — carrying total seconds-in-phase as a float
/// (e.g. `1.4493e-5`). [`parse_counter_value`] cannot read these: its
/// `value as u64` truncates any sub-second sum to zero. This sibling reads
/// the same line shape but converts seconds to nanoseconds — the unit
/// `elapsed_ns` already uses — so the value lands in the integer raw layer
/// without loss of the discipline ADR-005 set (no `f64` in raw types).
/// See ADR-012.
///
/// Only the `_sum` line is read; histogram `_bucket{le=...}` lines are not
/// parsed (ADR-011's histogram-bucket boundary is left intact, ADR-012).
///
/// The metric name match is exact on the text before `{`, and the
/// `model_name` label must equal `model_name`. A missing series, a
/// non-numeric value, or a negative/non-finite value is a `Parse` error.
fn parse_seconds_as_nanos(body: &str, metric: &str, model_name: &str) -> Result<u64, MetricsError> {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((series, value)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let series = series.trim();
        let value = value.trim();

        let (name, labels) = match series.split_once('{') {
            Some((name, rest)) => {
                let labels = rest.strip_suffix('}').unwrap_or(rest);
                (name, labels)
            }
            None => (series, ""),
        };

        if name != metric {
            continue;
        }
        if extract_label(labels, "model_name") != Some(model_name) {
            continue;
        }

        let seconds: f64 = value.parse().map_err(|_| MetricsError::Parse {
            detail: format!("metric {metric}: value {value:?} is not a number"),
        })?;
        if seconds < 0.0 || !seconds.is_finite() {
            return Err(MetricsError::Parse {
                detail: format!("metric {metric}: value {seconds} is not a valid duration"),
            });
        }
        return Ok((seconds * 1e9).round() as u64);
    }

    Err(MetricsError::Parse {
        detail: format!("metric {metric} not found for model_name {model_name:?}"),
    })
}

/// Reads a phase-time series the schema may not declare at all.
///
/// The two cases this separates are the whole of ADR-014 D3. A `None`
/// name is a declared capability gap: the engine has no such family, and
/// `Ok(None)` reports that without inventing a number. A declared name
/// that fails to turn up in the body is unchanged — still an error,
/// because the schema said it would be there.
fn parse_phase_time(
    body: &str,
    metric: Option<&'static str>,
    model_name: &str,
) -> Result<Option<u64>, MetricsError> {
    let Some(metric) = metric else {
        return Ok(None);
    };
    parse_seconds_as_nanos(body, metric, model_name).map(Some)
}

/// Reads a series the schema may not declare at all.
///
/// The sibling of [`parse_phase_time`] for a full [`Series`] rather than a
/// bare name, and it carries one more case. `parse_phase_time` treats an
/// undeclared family as a capability gap and a declared-but-absent one as a
/// defective body. For the speculative family that second half does not
/// hold: `SpecDecodingProm.__init__` returns before registering any counter
/// when the engine was started without a speculative config, so a vLLM
/// endpoint that is not speculating emits none of these lines while still
/// declaring them in its schema (ADR-016 D1).
///
/// Both absences therefore collapse onto `Ok(None)`: the engine has no such
/// capability, or it has it and this run did not enable it. From the
/// reader's side they are one fact — this run produced no speculative
/// measurement — and neither is a zero. `Err` keeps the meaning it has
/// everywhere else in this module: a line that exists and does not parse.
fn parse_optional_series(
    body: &str,
    series: Option<Series>,
    model_name: &str,
) -> Result<Option<u64>, MetricsError> {
    let Some(series) = series else {
        return Ok(None);
    };
    parse_series(body, &series, model_name)
}

/// One reading of the speculative-decoding family: draft tokens, accepted
/// tokens, rounds — in that order.
///
/// Named rather than left as a bare tuple for two reasons. The three are one
/// capability and travel together from the parser to
/// [`is_core::SpecTimeline::push`], so the trio is the unit the code actually
/// passes around. And they are three `Option<u64>` in a row: the compiler
/// cannot tell them apart, so a transposition at any call site would be
/// silent, and the report would carry an acceptance rate above one or an
/// acceptance length below one with nothing to flag it.
pub type SpecReading = (Option<u64>, Option<u64>, Option<u64>);

/// Parses the three speculative-decoding counters from a text-exposition
/// body (ADR-016).
///
/// Returns `(draft_tokens, accepted_tokens, drafts)`, each independently
/// optional. None of the three is required: the trio is one capability, and
/// deciding what a partial read means belongs to
/// [`is_core::SpecTimeline::push`], which drops the tick rather than let a
/// missing counter be read as a zero.
///
/// A body from an engine that does not speculate yields `(None, None, None)`
/// — the ordinary outcome on any run without a speculative config, not a
/// failure. This is the deliberate divergence from [`parse_phase`], whose
/// declared-but-absent series remains an error (ADR-016 D1).
pub fn parse_spec(
    body: &str,
    model_name: &str,
    engine: Engine,
) -> Result<SpecReading, MetricsError> {
    let schema = engine.schema();
    Ok((
        parse_optional_series(body, schema.spec_draft_tokens, model_name)?,
        parse_optional_series(body, schema.spec_accepted_tokens, model_name)?,
        parse_optional_series(body, schema.spec_drafts, model_name)?,
    ))
}

/// Parses both KV-cache counters from a text-exposition body.
///
/// Returns `(hits, queries)` for the given model, with the series named by
/// `engine`'s schema (ADR-014 D1).
///
/// The numerator is `Option`: a body can carry the denominator and no
/// numerator, and that asymmetry is real rather than a parse failure. On
/// SGLang the numerator family appears only once the prefix cache has been
/// hit, and a body carrying only the reserved `total` line yields no
/// numerator at all under [`Aggregation::SumOverLabel`]. Returning `None`
/// keeps the distinction the caller needs: a hit rate that cannot be
/// formed is not a hit rate of zero.
///
/// The denominator is required — without it no rate exists in either
/// direction — so its absence is a `Parse` error.
pub fn parse_kvcache(
    body: &str,
    model_name: &str,
    engine: Engine,
) -> Result<(Option<u64>, u64), MetricsError> {
    let schema = engine.schema();
    let hits = parse_series(body, &schema.hit_numerator, model_name)?;
    let queries = parse_series(body, &schema.hit_denominator, model_name)?;
    let queries = require(queries, schema.hit_denominator.name, model_name)?;
    Ok((hits, queries))
}

/// Parses the four phase signals from a text-exposition body (ADR-012).
///
/// Returns `(prompt_tokens, generation_tokens, prefill_ns, decode_ns)`:
/// the two phase token counters in their native integer form, and the two
/// phase-time `_sum` values converted from float seconds to integer
/// nanoseconds. The series are named by `engine`'s schema (ADR-014 D1).
///
/// The token counters are required — without them there is no phase split
/// in either basis. The timing legs are `Option` (ADR-014 D3): `None`
/// when the engine's schema declares no per-phase timing family, which is
/// a capability gap rather than a parse failure. A series the schema does
/// declare and the body does not carry remains an error.
pub fn parse_phase(
    body: &str,
    model_name: &str,
    engine: Engine,
) -> Result<(u64, u64, Option<u64>, Option<u64>), MetricsError> {
    let schema = engine.schema();

    let prompt_tokens = parse_series(body, &schema.prompt_tokens, model_name)?;
    let prompt_tokens = require(prompt_tokens, schema.prompt_tokens.name, model_name)?;

    let generation_tokens = parse_series(body, &schema.generation_tokens, model_name)?;
    let generation_tokens = require(generation_tokens, schema.generation_tokens.name, model_name)?;

    let prefill_ns = parse_phase_time(body, schema.prefill_time_sum, model_name)?;
    let decode_ns = parse_phase_time(body, schema.decode_time_sum, model_name)?;

    Ok((prompt_tokens, generation_tokens, prefill_ns, decode_ns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{SGLANG_SCHEMA, VLLM_SCHEMA};

    const FIXTURE: &str = include_str!("../tests/fixtures/llm-d-inference-sim-v0.8.2-metrics.txt");
    const SGLANG_PER_SOURCE: &str =
        include_str!("../tests/fixtures/sglang-tokenizer-metrics-per-source.txt");
    const SGLANG_TOTAL_ONLY: &str =
        include_str!("../tests/fixtures/sglang-tokenizer-metrics-total-only.txt");

    const SGLANG_MODEL: &str = "Qwen/Qwen2.5-7B-Instruct";
    const SGLANG: Engine = Engine::Sglang { page_size: 1 };

    #[test]
    fn extract_label_single() {
        let labels = r#"model_name="facebook/opt-125m""#;
        assert_eq!(
            extract_label(labels, "model_name"),
            Some("facebook/opt-125m")
        );
    }

    #[test]
    fn extract_label_multi_label_any_position() {
        let labels = r#"le="0.3",model_name="facebook/opt-125m",engine_type="unified""#;
        assert_eq!(
            extract_label(labels, "model_name"),
            Some("facebook/opt-125m")
        );
        assert_eq!(extract_label(labels, "le"), Some("0.3"));
        assert_eq!(extract_label(labels, "engine_type"), Some("unified"));
    }

    #[test]
    fn extract_label_absent_key_is_none() {
        let labels = r#"model_name="m""#;
        assert_eq!(extract_label(labels, "cache_source"), None);
    }

    #[test]
    fn extract_label_does_not_match_suffix_key() {
        let labels = r#"model_name="m""#;
        assert_eq!(extract_label(labels, "name"), None);
    }

    /// A body produced by `prometheus_client`, the library vLLM builds
    /// its counters with. This is the shape a real vLLM endpoint emits,
    /// and it differs from the sim fixture above in exactly the way that
    /// went unnoticed: the library appends `_total` on exposition, so a
    /// schema spelled with the name vLLM *registers* matches nothing.
    const VLLM_EXPOSITION: &str =
        include_str!("../tests/fixtures/vllm-prometheus-client-exposition.txt");

    /// The same shape, from a server started *with* a speculative config.
    /// Internally consistent with vLLM's own resolution: 128 drafts at k=5
    /// give 640 draft tokens, 384 accepted, so a realized mean acceptance
    /// length of exactly 4.0 including the bonus token (ADR-016 D4). The
    /// per-position lines sum to the accepted total.
    const VLLM_SPEC: &str = include_str!("../tests/fixtures/vllm-spec-decode-exposition.txt");

    const SPEC_MODEL: &str = "Qwen/Qwen2.5-7B-Instruct";

    #[test]
    fn kvcache_parses_against_a_prometheus_client_body() {
        let (hits, queries) =
            parse_kvcache(VLLM_EXPOSITION, "Qwen/Qwen2.5-7B-Instruct", Engine::Vllm).unwrap();
        assert_eq!(hits, Some(144));
        assert_eq!(queries, 270);
    }

    #[test]
    fn phase_parses_against_a_prometheus_client_body() {
        // The histogram legs were always spelled correctly; this pins
        // that the fix to the counters did not disturb them.
        let (prompt, generation, prefill_ns, decode_ns) =
            parse_phase(VLLM_EXPOSITION, "Qwen/Qwen2.5-7B-Instruct", Engine::Vllm).unwrap();
        assert_eq!(prompt, 196);
        assert_eq!(generation, 38);
        assert!(prefill_ns.is_some());
        assert!(decode_ns.is_some());
    }

    #[test]
    fn the_created_line_is_not_mistaken_for_the_counter() {
        // `prometheus_client` emits a `_created` gauge beside every
        // counter. An exact-name match must not read it as the series.
        assert!(VLLM_EXPOSITION.contains("vllm:prefix_cache_hits_created"));
        let v = parse_series(
            VLLM_EXPOSITION,
            &VLLM_SCHEMA.hit_numerator,
            "Qwen/Qwen2.5-7B-Instruct",
        )
        .unwrap();
        assert_eq!(v, Some(144));
    }

    /// `llm-d-inference-sim` is not a vLLM endpoint for the KV series,
    /// and this pins the divergence rather than papering over it.
    ///
    /// The simulator spells the token counters as vLLM *exposes* them
    /// (`vllm:generation_tokens_total`) but the prefix-cache counters as
    /// vLLM *registers* them (`vllm:prefix_cache_hits`, no suffix). Real
    /// vLLM builds both with `prometheus_client`, which appends `_total`
    /// on exposition. So a body from the simulator yields phase figures
    /// under `Engine::Vllm` and no KV rate at all.
    ///
    /// This asymmetry is why the KV schema went unverified: every test
    /// and every kind rehearsal read the simulator, and the simulator
    /// answered.
    #[test]
    fn the_simulator_is_not_a_vllm_endpoint_for_kv() {
        // Phase series: the simulator agrees with vLLM.
        let (prompt, generation, _, _) =
            parse_phase(FIXTURE, "facebook/opt-125m", Engine::Vllm).unwrap();
        assert_eq!(prompt, 196);
        assert_eq!(generation, 38);

        // KV series: it does not. The denominator is required, so this
        // is an error and never a hit rate of zero.
        let err = parse_kvcache(FIXTURE, "facebook/opt-125m", Engine::Vllm).unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }), "{err:?}");
    }

    #[test]
    fn parse_kvcache_wrong_model_name_errors() {
        // The denominator is required, so an unknown model is a parse
        // failure rather than an empty reading.
        let err = parse_kvcache(FIXTURE, "no/such-model", Engine::Vllm).unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_series_skips_comment_lines() {
        // A HELP line naming the metric must not be read as a sample.
        let body = "# HELP vllm:prefix_cache_hits_total Prefix cache hits.\n\
                    # TYPE vllm:prefix_cache_hits_total counter\n";
        let v = parse_series(body, &VLLM_SCHEMA.hit_numerator, "m").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn parse_series_exact_name_match() {
        // A longer name containing the metric must not match: this is
        // what keeps the _created line prometheus_client emits beside
        // every counter out of the reading.
        let body = "vllm:prefix_cache_hits_total_extra{model_name=\"m\"} 7\n";
        assert_eq!(
            parse_series(body, &VLLM_SCHEMA.hit_numerator, "m").unwrap(),
            None
        );
    }

    #[test]
    fn parse_series_reads_float_value_as_u64() {
        let body = "vllm:prefix_cache_hits_total{model_name=\"m\"} 9.0\n";
        let v = parse_series(body, &VLLM_SCHEMA.hit_numerator, "m").unwrap();
        assert_eq!(v, Some(9));
    }

    #[test]
    fn parse_series_rejects_negative() {
        // A line that exists but does not parse is an error, unlike a
        // line that is not there at all.
        let body = "vllm:prefix_cache_hits_total{model_name=\"m\"} -3\n";
        let err = parse_series(body, &VLLM_SCHEMA.hit_numerator, "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_series_absent_is_none_not_zero() {
        // The distinction the whole ADR-014 D4 numerator rests on.
        let body = "vllm:prefix_cache_queries_total{model_name=\"m\"} 100\n";
        assert_eq!(
            parse_series(body, &VLLM_SCHEMA.hit_numerator, "m").unwrap(),
            None
        );
    }

    #[test]
    fn sglang_numerator_sums_every_source_but_the_reserved_total() {
        // 1800 device + 400 host + 120 storage_hf3fs = 2320. The
        // cache_source="total" line carries 999 and is excluded; had it
        // been added the reading would be 3319, and had the sum been
        // whitelisted to device+host the unknown storage backend would
        // have been dropped silently at 2200.
        let n = parse_series(
            SGLANG_PER_SOURCE,
            &SGLANG_SCHEMA.hit_numerator,
            SGLANG_MODEL,
        )
        .unwrap();
        assert_eq!(n, Some(2320));
    }

    #[test]
    fn sglang_numerator_does_not_cross_models() {
        // The second model's device line carries 55 and must not leak
        // into the sum for the first.
        let n = parse_series(
            SGLANG_PER_SOURCE,
            &SGLANG_SCHEMA.hit_numerator,
            "meta-llama/Llama-3.1-8B",
        )
        .unwrap();
        assert_eq!(n, Some(55));
    }

    #[test]
    fn sglang_total_only_body_yields_an_absent_numerator() {
        // The backward-compatible path: every cached token is under the
        // reserved value, so there is nothing to sum. Absence here must
        // not be readable as a zero hit rate on a server that is in fact
        // serving 2320 cached tokens out of 3400.
        let (hits, queries) = parse_kvcache(SGLANG_TOTAL_ONLY, SGLANG_MODEL, SGLANG).unwrap();
        assert_eq!(hits, None);
        assert_eq!(queries, 3400);
    }

    #[test]
    fn sglang_token_counters_sum_across_is_streaming() {
        // The defect this pins: SGLang declares the two token counters with
        // `labelnames=list(labels.keys()) + ["is_streaming"]`
        // (observability/metrics_collector.py:1519-1528), so a server that
        // answers both streaming and non-streaming requests emits each as two
        // lines. Under Aggregation::Single the parser returned on the first
        // match and reported a part as the whole — the hit-rate denominator
        // short by the other line, and the rate correspondingly inflated.
        //
        // The fixture carries 2900 streaming and 500 non-streaming prompt
        // tokens: reading either alone is wrong, and which one you get
        // depends on exposition order.
        let prompt = parse_series(
            SGLANG_PER_SOURCE,
            &SGLANG_SCHEMA.prompt_tokens,
            SGLANG_MODEL,
        )
        .unwrap();
        assert_eq!(prompt, Some(3400), "2900 streaming + 500 non-streaming");

        let generation = parse_series(
            SGLANG_PER_SOURCE,
            &SGLANG_SCHEMA.generation_tokens,
            SGLANG_MODEL,
        )
        .unwrap();
        assert_eq!(generation, Some(512), "480 streaming + 32 non-streaming");

        // And the sum stays inside one model: the second model carries
        // 60 + 17 prompt tokens and must not leak into the first.
        let other = parse_series(
            SGLANG_PER_SOURCE,
            &SGLANG_SCHEMA.prompt_tokens,
            "meta-llama/Llama-3.1-8B",
        )
        .unwrap();
        assert_eq!(other, Some(77));
    }

    #[test]
    fn vllm_token_counters_carry_no_splitting_label() {
        // The reason vLLM keeps Single: its counters are declared with a bare
        // `labelnames=labelnames` (v1/metrics/loggers.py:585,596,671,705),
        // with nothing appended. Checked at source rather than assumed,
        // because the same defect on vLLM would be the one that matters.
        assert_eq!(VLLM_SCHEMA.prompt_tokens.aggregation, Aggregation::Single);
        assert_eq!(
            VLLM_SCHEMA.generation_tokens.aggregation,
            Aggregation::Single
        );
        assert_eq!(VLLM_SCHEMA.hit_denominator.aggregation, Aggregation::Single);
    }

    #[test]
    fn sglang_denominator_is_the_prompt_counter() {
        // The collision D1 exists for: one series serving two roles.
        let (_, queries) = parse_kvcache(SGLANG_PER_SOURCE, SGLANG_MODEL, SGLANG).unwrap();
        let prompt = parse_series(
            SGLANG_PER_SOURCE,
            &SGLANG_SCHEMA.prompt_tokens,
            SGLANG_MODEL,
        )
        .unwrap()
        .unwrap();
        assert_eq!(queries, 3400);
        assert_eq!(queries, prompt);
    }

    #[test]
    fn sglang_created_lines_are_not_read_as_counters() {
        // prometheus_client emits sglang:cached_tokens_created beside
        // the counter. Its value is a unix timestamp, so reading it
        // would not look obviously wrong in a report.
        let n = parse_series(
            SGLANG_PER_SOURCE,
            &SGLANG_SCHEMA.hit_numerator,
            SGLANG_MODEL,
        )
        .unwrap();
        assert_eq!(n, Some(2320));
        assert!(SGLANG_PER_SOURCE.contains("sglang:cached_tokens_created"));
    }

    #[test]
    fn vllm_schema_does_not_read_an_sglang_body() {
        // The reason main withholds an --engine flag until this point:
        // the wrong vocabulary finds nothing, and nothing must not be
        // rendered as a number.
        let err = parse_kvcache(SGLANG_PER_SOURCE, SGLANG_MODEL, Engine::Vllm).unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_seconds_as_nanos_prefill_from_real_fixture() {
        let ns = parse_seconds_as_nanos(
            FIXTURE,
            VLLM_SCHEMA.prefill_time_sum.unwrap(),
            "facebook/opt-125m",
        )
        .unwrap();
        // 1.4493e-05 s = 14493 ns.
        assert_eq!(ns, 14_493);
    }

    #[test]
    fn parse_seconds_as_nanos_decode_from_real_fixture() {
        let ns = parse_seconds_as_nanos(
            FIXTURE,
            VLLM_SCHEMA.decode_time_sum.unwrap(),
            "facebook/opt-125m",
        )
        .unwrap();
        // 2.8431999999999998e-05 s, rounded to the nearest nanosecond.
        assert_eq!(ns, 28_432);
    }

    #[test]
    fn parse_seconds_as_nanos_does_not_truncate_subsecond() {
        // The reason this sibling exists: as u64 on seconds would be 0.
        let body = "vllm:request_prefill_time_seconds_sum{model_name=\"m\"} 1.4493e-05\n";
        let ns =
            parse_seconds_as_nanos(body, "vllm:request_prefill_time_seconds_sum", "m").unwrap();
        assert_eq!(ns, 14_493);
    }

    #[test]
    fn parse_seconds_as_nanos_missing_series_errors() {
        let err =
            parse_seconds_as_nanos("", "vllm:request_prefill_time_seconds_sum", "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_seconds_as_nanos_rejects_negative() {
        let body = "vllm:request_prefill_time_seconds_sum{model_name=\"m\"} -1.0\n";
        let err =
            parse_seconds_as_nanos(body, "vllm:request_prefill_time_seconds_sum", "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_seconds_as_nanos_exact_name_match() {
        // The _bucket and _count lines of the same family must not match.
        let body = "vllm:request_prefill_time_seconds_count{model_name=\"m\"} 4\n";
        let err =
            parse_seconds_as_nanos(body, "vllm:request_prefill_time_seconds_sum", "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_phase_from_real_fixture() {
        let (prompt_tokens, generation_tokens, prefill_ns, decode_ns) =
            parse_phase(FIXTURE, "facebook/opt-125m", Engine::Vllm).unwrap();
        assert_eq!(prompt_tokens, 196);
        assert_eq!(generation_tokens, 38);
        assert_eq!(prefill_ns, Some(14_493));
        assert_eq!(decode_ns, Some(28_432));
    }

    #[test]
    fn parse_phase_missing_a_declared_series_still_errors() {
        // The other side of ADR-014 D3: the vLLM schema declares the
        // timing families, so a body that does not carry them is a
        // defective body, not an engine without the capability.
        let body = "vllm:prompt_tokens_total{model_name=\"m\"} 10\n\
                    vllm:generation_tokens_total{model_name=\"m\"} 5\n";
        let err = parse_phase(body, "m", Engine::Vllm).unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_phase_on_an_engine_without_timing_yields_absence_not_error() {
        // SGLang exposes no phase-separated timing counters (ADR-014
        // D3). The tokens parse; the timing legs come back absent, and
        // absence is not zero — a zero here would be a measurement the
        // engine never took.
        let (prompt_tokens, generation_tokens, prefill_ns, decode_ns) =
            parse_phase(SGLANG_PER_SOURCE, SGLANG_MODEL, SGLANG).unwrap();
        assert_eq!(prompt_tokens, 3400);
        assert_eq!(generation_tokens, 512);
        assert_eq!(prefill_ns, None);
        assert_eq!(decode_ns, None);
    }

    #[test]
    fn spec_reads_the_three_counters() {
        let (draft, accepted, drafts) = parse_spec(VLLM_SPEC, SPEC_MODEL, Engine::Vllm).unwrap();
        assert_eq!(draft, Some(640));
        assert_eq!(accepted, Some(384));
        assert_eq!(drafts, Some(128));
    }

    #[test]
    fn spec_on_a_non_speculating_vllm_is_absence_not_error() {
        // The divergence from parse_phase, and the whole of ADR-016 D1.
        // SpecDecodingProm.__init__ returns before registering anything
        // when speculative_config is None, so this body — a real vLLM
        // exposition — carries none of the three. That is a server not
        // speculating, not a defective body.
        let (draft, accepted, drafts) =
            parse_spec(VLLM_EXPOSITION, "Qwen/Qwen2.5-7B-Instruct", Engine::Vllm).unwrap();
        assert_eq!(draft, None);
        assert_eq!(accepted, None);
        assert_eq!(drafts, None);
    }

    #[test]
    fn the_same_body_still_errors_on_a_declared_phase_series() {
        // The other half of D1: the divergence is scoped to the
        // speculative family. parse_phase keeps its contract on the very
        // body parse_spec reads as absent, so this is not a general
        // loosening of the schema's promise.
        let body = "vllm:prompt_tokens_total{model_name=\"m\"} 10\n\
                    vllm:generation_tokens_total{model_name=\"m\"} 5\n";
        assert!(parse_spec(body, "m", Engine::Vllm).unwrap() == (None, None, None));
        assert!(matches!(
            parse_phase(body, "m", Engine::Vllm).unwrap_err(),
            MetricsError::Parse { .. }
        ));
    }

    #[test]
    fn spec_on_sglang_is_absence_by_schema() {
        // The other absence that collapses onto Ok(None): SGLang exposes
        // the family as gauges, which window differencing cannot read, so
        // the schema declares None (ADR-014, ADR-016 D1).
        let (draft, accepted, drafts) =
            parse_spec(SGLANG_PER_SOURCE, SGLANG_MODEL, SGLANG).unwrap();
        assert_eq!(draft, None);
        assert_eq!(accepted, None);
        assert_eq!(drafts, None);
    }

    #[test]
    fn the_per_pos_family_is_not_read_as_accepted_tokens() {
        // Same trap _created posed, same defence: the exact match on the
        // text before `{` excludes a longer name. Reading the per-position
        // family here would report 128 accepted tokens instead of 384,
        // because Aggregation::Single takes the first match (ADR-016 D5).
        assert!(VLLM_SPEC.contains("vllm:spec_decode_num_accepted_tokens_per_pos_total"));
        let v = parse_series(
            VLLM_SPEC,
            &VLLM_SCHEMA.spec_accepted_tokens.unwrap(),
            SPEC_MODEL,
        )
        .unwrap();
        assert_eq!(v, Some(384));
    }

    #[test]
    fn spec_created_lines_are_not_read_as_counters() {
        assert!(VLLM_SPEC.contains("vllm:spec_decode_num_drafts_created"));
        let (_, _, drafts) = parse_spec(VLLM_SPEC, SPEC_MODEL, Engine::Vllm).unwrap();
        assert_eq!(drafts, Some(128));
    }

    #[test]
    fn spec_does_not_cross_models() {
        // The second model on the body carries 45/27/9 and must not leak.
        let (draft, accepted, drafts) =
            parse_spec(VLLM_SPEC, "meta-llama/Llama-3.1-8B", Engine::Vllm).unwrap();
        assert_eq!(draft, Some(45));
        assert_eq!(accepted, Some(27));
        assert_eq!(drafts, Some(9));
    }
}
