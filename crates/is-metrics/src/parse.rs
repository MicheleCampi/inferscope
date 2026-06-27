//! Pure parsing of Prometheus text-exposition content.
//!
//! This module is I/O-free, mirroring [`is_sysmon`]'s `parse` module:
//! the scrape loop reads the HTTP body, hands the text here, and this
//! code turns it into counter values. No network, no async.
//!
//! Scope is deliberately narrow (ADR-011): the two KV-cache counter
//! series, `vllm:prefix_cache_hits` and `vllm:prefix_cache_queries`,
//! selected by their `model_name` label. Histogram series (`_bucket`,
//! `_sum`, `_count`) and other metrics are ignored — an exact match on
//! the full metric name excludes them. Extending to histograms would be
//! parser work this module does not yet do; that trade-off is recorded
//! in ADR-011.

use crate::error::MetricsError;

/// The two KV-cache counter series this crate reads.
const METRIC_HITS: &str = "vllm:prefix_cache_hits";
const METRIC_QUERIES: &str = "vllm:prefix_cache_queries";
/// The phase token counters (ADR-012): cumulative prompt (prefill) and
/// generation (decode) token totals.
const METRIC_PROMPT_TOKENS: &str = "vllm:prompt_tokens_total";
const METRIC_GENERATION_TOKENS: &str = "vllm:generation_tokens_total";
/// The phase time histogram `_sum` series (ADR-012): total seconds spent
/// in prefill and in decode. Read via `parse_seconds_as_nanos`.
const METRIC_PREFILL_TIME_SUM: &str = "vllm:request_prefill_time_seconds_sum";
const METRIC_DECODE_TIME_SUM: &str = "vllm:request_decode_time_seconds_sum";

/// Extracts the value of one label from a Prometheus label block.
///
/// `labels` is the raw content between the braces, without the braces
/// themselves: e.g. `model_name="facebook/opt-125m",le="0.3"`. Returns
/// the value of `key` (the text inside the quotes) if present.
///
/// This handles the multi-label case: `model_name` may not be the only
/// label, nor the first. The search is for `key="` followed by the
/// value up to the closing quote, so label order does not matter.
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

/// Parses one counter series from a text-exposition body.
///
/// Finds the line whose metric name is exactly `metric` and whose
/// `model_name` label equals `model_name`, and returns its value.
///
/// The value is read as `f64` then converted to `u64`: the Prometheus
/// wire format may render any value in float or scientific notation even
/// for a counter (the sim emits e.g. `0.001509284` for latency sums), so
/// reading as `f64` is robust where reading as `u64` directly would not
/// be. A counter is conceptually a non-negative integer; the conversion
/// truncates toward zero, which is exact for the integer values these
/// counters actually hold.
///
/// Lines beginning with `#` (HELP, TYPE, and documentary comments) are
/// skipped. The metric name match is exact on the text before `{`, so
/// `prefix_cache_hits` never matches a longer name that contains it.
fn parse_counter(body: &str, metric: &str, model_name: &str) -> Result<u64, MetricsError> {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split into "name{labels}" and "value" on the last whitespace.
        let Some((series, value)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let series = series.trim();
        let value = value.trim();

        // Separate the metric name from the label block.
        let (name, labels) = match series.split_once('{') {
            Some((name, rest)) => {
                let labels = rest.strip_suffix('}').unwrap_or(rest);
                (name, labels)
            }
            // A series with no labels: name is the whole thing. Not the
            // shape our counters take, but handled rather than panicked.
            None => (series, ""),
        };

        if name != metric {
            continue;
        }
        if extract_label(labels, "model_name") != Some(model_name) {
            continue;
        }

        let parsed: f64 = value.parse().map_err(|_| MetricsError::Parse {
            detail: format!("metric {metric}: value {value:?} is not a number"),
        })?;
        if parsed < 0.0 || !parsed.is_finite() {
            return Err(MetricsError::Parse {
                detail: format!("metric {metric}: value {parsed} is not a valid counter"),
            });
        }
        return Ok(parsed as u64);
    }

    Err(MetricsError::Parse {
        detail: format!("metric {metric} not found for model_name {model_name:?}"),
    })
}

/// Parses one histogram `_sum` series as integer nanoseconds.
///
/// Phase timing in the vLLM schema is exposed as histogram families
/// (`request_prefill_time_seconds`, `request_decode_time_seconds`); the
/// `_sum` line of each is a single `metric{labels} value` line — the same
/// shape a counter takes — carrying total seconds-in-phase as a float
/// (e.g. `1.4493e-5`). [`parse_counter`] cannot read these: its `value as
/// u64` truncates any sub-second sum to zero. This sibling reads the same
/// line shape but converts seconds to nanoseconds — the unit `elapsed_ns`
/// already uses — so the value lands in the integer raw layer without loss
/// of the discipline ADR-005 set (no `f64` in raw types). See ADR-012.
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

/// Parses both KV-cache counters from a text-exposition body.
///
/// Returns `(hits, queries)` — the raw cumulative values of
/// `vllm:prefix_cache_hits` and `vllm:prefix_cache_queries` for the
/// given model. Both must be present; a missing series is an error,
/// since a hit rate cannot be formed without both.
pub fn parse_kvcache(body: &str, model_name: &str) -> Result<(u64, u64), MetricsError> {
    let hits = parse_counter(body, METRIC_HITS, model_name)?;
    let queries = parse_counter(body, METRIC_QUERIES, model_name)?;
    Ok((hits, queries))
}

/// Parses the four phase signals from a text-exposition body (ADR-012).
///
/// Returns `(prompt_tokens, generation_tokens, prefill_ns, decode_ns)`:
/// the two phase token counters in their native integer form, and the two
/// phase-time `_sum` values converted from float seconds to integer
/// nanoseconds. All four must be present; a missing series is an error,
/// since a phase split cannot be formed without all of them.
///
/// The two token series are read with [`parse_counter`] (they are plain
/// integer counters); the two time series with [`parse_seconds_as_nanos`]
/// (their sub-second float sums would truncate to zero under `parse_counter`).
pub fn parse_phase(body: &str, model_name: &str) -> Result<(u64, u64, u64, u64), MetricsError> {
    let prompt_tokens = parse_counter(body, METRIC_PROMPT_TOKENS, model_name)?;
    let generation_tokens = parse_counter(body, METRIC_GENERATION_TOKENS, model_name)?;
    let prefill_ns = parse_seconds_as_nanos(body, METRIC_PREFILL_TIME_SUM, model_name)?;
    let decode_ns = parse_seconds_as_nanos(body, METRIC_DECODE_TIME_SUM, model_name)?;
    Ok((prompt_tokens, generation_tokens, prefill_ns, decode_ns))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/llm-d-inference-sim-v0.8.2-metrics.txt");

    #[test]
    fn extract_label_single() {
        assert_eq!(
            extract_label(r#"model_name="facebook/opt-125m""#, "model_name"),
            Some("facebook/opt-125m")
        );
    }

    #[test]
    fn extract_label_multi_label_any_position() {
        let labels = r#"model_name="facebook/opt-125m",le="0.3""#;
        assert_eq!(
            extract_label(labels, "model_name"),
            Some("facebook/opt-125m")
        );
        assert_eq!(extract_label(labels, "le"), Some("0.3"));
    }

    #[test]
    fn extract_label_absent_key_is_none() {
        assert_eq!(extract_label(r#"le="0.3""#, "model_name"), None);
    }

    #[test]
    fn extract_label_does_not_match_suffix_key() {
        // Searching "name" must not match "model_name".
        assert_eq!(extract_label(r#"model_name="x""#, "name"), None);
    }

    #[test]
    fn parse_kvcache_from_real_fixture() {
        // The authoritative Blocco A fixture: hits=96, queries=196 for
        // facebook/opt-125m on the v0.8.2 sim.
        let (hits, queries) = parse_kvcache(FIXTURE, "facebook/opt-125m").unwrap();
        assert_eq!(hits, 96);
        assert_eq!(queries, 196);
    }

    #[test]
    fn parse_kvcache_wrong_model_name_errors() {
        let err = parse_kvcache(FIXTURE, "no/such-model").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_counter_skips_comment_lines() {
        // A body where the only occurrence of the metric name is inside
        // a HELP comment must not be parsed as a value.
        let body =
            "# HELP vllm:prefix_cache_hits some text\n# TYPE vllm:prefix_cache_hits counter\n";
        let err = parse_counter(body, METRIC_HITS, "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_counter_exact_name_match() {
        // A longer metric name containing the target as a prefix must
        // not be matched.
        let body = "vllm:prefix_cache_hits_total{model_name=\"m\"} 5\n";
        let err = parse_counter(body, METRIC_HITS, "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_counter_reads_float_value_as_u64() {
        // Robustness: a counter rendered in float/scientific notation
        // must still parse. Truncation toward zero is exact for the
        // integer the counter holds.
        let body = "vllm:prefix_cache_hits{model_name=\"m\"} 9.6e1\n";
        let v = parse_counter(body, METRIC_HITS, "m").unwrap();
        assert_eq!(v, 96);
    }

    #[test]
    fn parse_counter_rejects_negative() {
        let body = "vllm:prefix_cache_hits{model_name=\"m\"} -1\n";
        let err = parse_counter(body, METRIC_HITS, "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_seconds_as_nanos_prefill_from_real_fixture() {
        // request_prefill_time_seconds_sum = 1.4493e-5 s -> 14493 ns.
        let ns = parse_seconds_as_nanos(
            FIXTURE,
            "vllm:request_prefill_time_seconds_sum",
            "facebook/opt-125m",
        )
        .unwrap();
        assert_eq!(ns, 14493);
    }

    #[test]
    fn parse_seconds_as_nanos_decode_from_real_fixture() {
        // request_decode_time_seconds_sum = 2.8432e-5 s -> 28432 ns.
        let ns = parse_seconds_as_nanos(
            FIXTURE,
            "vllm:request_decode_time_seconds_sum",
            "facebook/opt-125m",
        )
        .unwrap();
        assert_eq!(ns, 28432);
    }

    #[test]
    fn parse_seconds_as_nanos_does_not_truncate_subsecond() {
        // The bug parse_counter would have: 1.4493e-5 as u64 == 0.
        // The sibling must preserve it as nanoseconds.
        let body = "vllm:request_prefill_time_seconds_sum{model_name=\"m\"} 1.4493e-5\n";
        let ns =
            parse_seconds_as_nanos(body, "vllm:request_prefill_time_seconds_sum", "m").unwrap();
        assert_eq!(ns, 14493);
    }

    #[test]
    fn parse_seconds_as_nanos_missing_series_errors() {
        let err = parse_seconds_as_nanos(FIXTURE, "vllm:no_such_seconds_sum", "facebook/opt-125m")
            .unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_seconds_as_nanos_rejects_negative() {
        let body = "vllm:request_prefill_time_seconds_sum{model_name=\"m\"} -1\n";
        let err =
            parse_seconds_as_nanos(body, "vllm:request_prefill_time_seconds_sum", "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }

    #[test]
    fn parse_seconds_as_nanos_exact_name_match() {
        // A longer name containing the target as a prefix must not match.
        let body = "vllm:request_prefill_time_seconds_sum_extra{model_name=\"m\"} 1.0\n";
        let err =
            parse_seconds_as_nanos(body, "vllm:request_prefill_time_seconds_sum", "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }
    #[test]
    fn parse_phase_from_real_fixture() {
        // Fixture: prompt_tokens=196, generation_tokens=38,
        // prefill_sum=1.4493e-5 s -> 14493 ns, decode_sum=2.8432e-5 s -> 28432 ns.
        let (prompt, generation, prefill_ns, decode_ns) =
            parse_phase(FIXTURE, "facebook/opt-125m").unwrap();
        assert_eq!(prompt, 196);
        assert_eq!(generation, 38);
        assert_eq!(prefill_ns, 14493);
        assert_eq!(decode_ns, 28432);
    }

    #[test]
    fn parse_phase_missing_any_series_errors() {
        // A body with only the token counters, no phase times, must error.
        let body = "vllm:prompt_tokens_total{model_name=\"m\"} 10\nvllm:generation_tokens_total{model_name=\"m\"} 5\n";
        let err = parse_phase(body, "m").unwrap_err();
        assert!(matches!(err, MetricsError::Parse { .. }));
    }
}
