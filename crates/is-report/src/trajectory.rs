//! Trajectory step ingestion (ADR-013).
//!
//! An agentic trajectory is a sequence of steps — LLM calls and tool
//! executions — demarcated by the driver that creates them. The driver
//! emits a JSONL file of step boundaries; this module parses it.
//!
//! Parsing is structural only: malformed JSON, unknown kinds, inverted
//! windows, and duplicate ids are errors of the input file itself.
//! Semantic placement against the run window (steps outside the run,
//! overlapping neighbours) is judged during derivation, where the run
//! window is known, and is reported as dropped-step diagnostics rather
//! than parse failures.
//!
//! Per the crate contract this module does no I/O: it parses the file
//! *content*, handed in by the caller.

use serde::{Deserialize, Serialize};

/// What a step did: talked to the model, or ran a tool.
///
/// Tool steps are first-class even though no tokens flow during them:
/// their windows carry device energy (idle draw, or whatever the tool
/// itself puts on the device), and the cost of the agent *not* talking
/// to the model is part of the per-task story (ADR-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// An LLM call against the serving endpoint.
    LlmCall,
    /// A tool execution outside the engine.
    Tool,
}

/// One step boundary as emitted by the driver.
///
/// Timestamps are UTC unix-epoch nanoseconds read from the driver's
/// wall clock at the boundary instants. The schema is deliberately
/// minimal and framework-agnostic: any driver that can write four
/// fields to a file can produce it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    /// Driver-assigned step identifier, unique within the file.
    pub step_id: u64,
    /// Whether the step was an LLM call or a tool execution.
    pub kind: StepKind,
    /// Wall-clock start of the step, UTC unix-epoch nanoseconds.
    pub t_start_unix_ns: u64,
    /// Wall-clock end of the step, UTC unix-epoch nanoseconds.
    pub t_end_unix_ns: u64,
}

/// Structural failures of the step file.
///
/// Every variant names the 1-based line it occurred on: the file is
/// user-supplied input, and an error without a location is a puzzle.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StepFileError {
    /// A line was not a valid step object.
    #[error("line {line}: not a valid step record: {message}")]
    Malformed { line: usize, message: String },
    /// A step ended before it started.
    #[error("line {line}: step {step_id} ends before it starts (t_end {t_end_unix_ns} < t_start {t_start_unix_ns})")]
    InvertedWindow {
        line: usize,
        step_id: u64,
        t_start_unix_ns: u64,
        t_end_unix_ns: u64,
    },
    /// The same step id appeared twice.
    #[error("line {line}: duplicate step_id {step_id} (first seen on line {first_line})")]
    DuplicateId {
        line: usize,
        first_line: usize,
        step_id: u64,
    },
}

/// Parses the content of a driver step file (JSONL, one step object
/// per line; blank lines are skipped).
///
/// Returns the steps in file order. Ordering, placement against the
/// run window, and overlap between neighbours are judged at
/// derivation time, not here.
pub fn parse_steps(content: &str) -> Result<Vec<StepRecord>, StepFileError> {
    let mut steps: Vec<StepRecord> = Vec::new();
    // step_id -> 1-based line it was first seen on, for duplicate
    // reporting.
    let mut seen: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (idx, raw) in content.lines().enumerate() {
        let line = idx + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: StepRecord =
            serde_json::from_str(trimmed).map_err(|e| StepFileError::Malformed {
                line,
                message: e.to_string(),
            })?;
        if record.t_end_unix_ns < record.t_start_unix_ns {
            return Err(StepFileError::InvertedWindow {
                line,
                step_id: record.step_id,
                t_start_unix_ns: record.t_start_unix_ns,
                t_end_unix_ns: record.t_end_unix_ns,
            });
        }
        if let Some(&first_line) = seen.get(&record.step_id) {
            return Err(StepFileError::DuplicateId {
                line,
                first_line,
                step_id: record.step_id,
            });
        }
        seen.insert(record.step_id, line);
        steps.push(record);
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_llm_and_tool_steps_in_order() {
        let content = r#"{"step_id": 1, "kind": "llm_call", "t_start_unix_ns": 100, "t_end_unix_ns": 200}
{"step_id": 2, "kind": "tool", "t_start_unix_ns": 200, "t_end_unix_ns": 350}"#;
        let steps = parse_steps(content).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].kind, StepKind::LlmCall);
        assert_eq!(steps[1].kind, StepKind::Tool);
        assert_eq!(steps[1].t_end_unix_ns, 350);
    }

    #[test]
    fn skips_blank_lines() {
        let content = "\n{\"step_id\": 1, \"kind\": \"tool\", \"t_start_unix_ns\": 1, \"t_end_unix_ns\": 2}\n\n";
        assert_eq!(parse_steps(content).unwrap().len(), 1);
    }

    #[test]
    fn zero_length_window_is_structurally_valid() {
        let content = r#"{"step_id": 1, "kind": "tool", "t_start_unix_ns": 5, "t_end_unix_ns": 5}"#;
        assert_eq!(parse_steps(content).unwrap().len(), 1);
    }

    #[test]
    fn unknown_kind_is_malformed_with_line_number() {
        let content = r#"{"step_id": 1, "kind": "llm_call", "t_start_unix_ns": 1, "t_end_unix_ns": 2}
{"step_id": 2, "kind": "banana", "t_start_unix_ns": 3, "t_end_unix_ns": 4}"#;
        match parse_steps(content) {
            Err(StepFileError::Malformed { line, .. }) => assert_eq!(line, 2),
            other => panic!("expected Malformed on line 2, got {other:?}"),
        }
    }

    #[test]
    fn inverted_window_is_rejected() {
        let content =
            r#"{"step_id": 7, "kind": "tool", "t_start_unix_ns": 10, "t_end_unix_ns": 9}"#;
        match parse_steps(content) {
            Err(StepFileError::InvertedWindow { step_id, line, .. }) => {
                assert_eq!(step_id, 7);
                assert_eq!(line, 1);
            }
            other => panic!("expected InvertedWindow, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_id_names_both_lines() {
        let content = r#"{"step_id": 1, "kind": "tool", "t_start_unix_ns": 1, "t_end_unix_ns": 2}
{"step_id": 1, "kind": "llm_call", "t_start_unix_ns": 3, "t_end_unix_ns": 4}"#;
        match parse_steps(content) {
            Err(StepFileError::DuplicateId {
                line, first_line, ..
            }) => {
                assert_eq!((first_line, line), (1, 2));
            }
            other => panic!("expected DuplicateId, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Derived layer (ADR-013): step windows sliced from Report timelines.
// ---------------------------------------------------------------------------

/// Why a step was excluded from attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// The step's window falls partly outside the run window (the
    /// span covered by GPU samples), or starts before the run's
    /// wall-clock anchor.
    OutsideRunWindow,
    /// The step overlaps the preceding kept step. Overlapping windows
    /// would double-count energy — the one error this design must
    /// never commit (ADR-013) — so the later step is dropped.
    OverlapsPrecedingStep,
}

/// Diagnostic record of a dropped step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedStep {
    /// The driver-assigned id of the dropped step.
    pub step_id: u64,
    /// Why it was dropped.
    pub reason: DropReason,
}

/// Per-step figures sliced from the run's timelines (ADR-013).
///
/// Integer raw deltas; the only floats are the derived ratios at the
/// edge, per the house discipline. `samples_in_window` declares the
/// grid resolution of the window instead of hiding it behind
/// interpolation: a step shorter than the sample period yields at
/// most one bracketing interval, and that fact is visible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepMetrics {
    /// Driver-assigned step identifier.
    pub step_id: u64,
    /// Whether the step was an LLM call or a tool execution.
    pub kind: StepKind,
    /// Step start in Report coordinates (ns from the reference instant).
    pub start_elapsed_ns: u64,
    /// Step end in Report coordinates (ns from the reference instant).
    pub end_elapsed_ns: u64,
    /// GPU samples (across all devices) whose `elapsed_ns` falls
    /// within the step window, boundaries inclusive.
    pub samples_in_window: u64,
    /// Energy over the step window in millijoules: trapezoidal
    /// integral of sampled power over the inter-sample segments fully
    /// contained in the window, summed per device (ADR-013; same
    /// integration basis as the ADR-010 fallback).
    pub energy_mj: u64,
    /// Generation-token delta over the window (phase timeline,
    /// ADR-012). `None` when no phase timeline was scraped: an
    /// unobserved counter is absence, not a measured zero.
    pub generation_tokens_delta: Option<u64>,
    /// Prompt-token delta over the window (phase timeline, ADR-012).
    /// `None` when no phase timeline was scraped.
    pub prompt_tokens_delta: Option<u64>,
    /// KV-cache hit delta over the window (ADR-011). `None` when no
    /// KV-cache timeline was scraped.
    pub cache_hits_delta: Option<u64>,
    /// KV-cache query delta over the window (ADR-011). `None` when no
    /// KV-cache timeline was scraped.
    pub cache_queries_delta: Option<u64>,
    /// Generation tokens per joule over the step window. `None` for
    /// tool steps and for zero tokens or zero energy: "no tokens" is
    /// absence, not a measured zero efficiency (ADR-013).
    pub tokens_per_joule: Option<f64>,
    /// Cache hit rate over the step window. `None` when
    /// `cache_queries_delta == 0`.
    pub cache_hit_rate: Option<f64>,
}

/// Whole-trajectory attribution derived from step windows (ADR-013).
///
/// `unattributed_energy_mj` is load-bearing: steps need not tile the
/// run, and the energy outside every step window is its own figure
/// instead of silently vanishing. Steps plus unattributed reconcile
/// to `total_energy_mj` exactly — all three figures come from the
/// same integer segment accounting, each inter-sample segment counted
/// at most once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryMetrics {
    /// Per-step figures, in window order.
    pub steps: Vec<StepMetrics>,
    /// Whole-run energy in millijoules on the trajectory layer's
    /// integration basis (trapezoidal over all inter-sample
    /// segments, summed per device).
    pub total_energy_mj: u64,
    /// Whole-run generation-token delta (phase timeline). Zero when
    /// no phase timeline was scraped.
    pub total_generation_tokens: u64,
    /// Whole-run generation tokens per joule. `None` for zero tokens
    /// or zero energy.
    pub trajectory_tokens_per_joule: Option<f64>,
    /// Sum of `energy_mj` over LLM-call steps.
    pub llm_energy_mj: u64,
    /// Sum of `energy_mj` over tool steps.
    pub tool_energy_mj: u64,
    /// Run energy not inside any kept step window.
    pub unattributed_energy_mj: u64,
    /// Steps excluded from attribution, with reasons (ADR-013).
    pub dropped_steps: Vec<DroppedStep>,
}

/// Doubled trapezoid area over the inter-sample segments of one
/// device's samples, in mW·ns: Σ (p_a + p_b) · Δt. Millijoules are
/// `area2 / 2_000_000_000` (integer floor). Integer accounting is
/// what makes the reconciliation exact: with disjoint step windows,
/// Σ floor(step) ≤ floor(total) always holds.
///
/// `window = Some((s, e))` restricts to segments whose *both*
/// endpoints fall in `[s, e]`; segments straddling a boundary belong
/// to no step and land in the unattributed remainder.
fn area2_mw_ns(samples: &[&is_core::GpuSample], window: Option<(u64, u64)>) -> u128 {
    let mut area2: u128 = 0;
    for pair in samples.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if let Some((s, e)) = window {
            if a.elapsed_ns < s || b.elapsed_ns > e {
                continue;
            }
        }
        let dt = b.elapsed_ns.saturating_sub(a.elapsed_ns) as u128;
        area2 += (a.power_draw_milliwatts as u128 + b.power_draw_milliwatts as u128) * dt;
    }
    area2
}

/// First and last sample whose elapsed time falls in `[s, e]`
/// (boundaries inclusive), by binary search over `elapsed_ns` —
/// the ADR-003 access pattern. `None` when the window contains no
/// sample. A single-sample window yields `first == last`, so every
/// counter delta over it is zero.
/// Baseline and end samples for a counter delta over `[s, e]`.
///
/// The end is the last sample at or before `e`. The baseline is the
/// last sample at or *before* `s` when one exists, not the first
/// sample inside the window: a counter that jumps between `s` and the
/// first interior sample (prompt tokens on prefill) would otherwise
/// read as zero, and a progressive counter would under-report by the
/// same gap. With no sample at or before `s` the first interior
/// sample is the only available baseline and the pre-window increment
/// is unrecoverable.
///
/// The cost of this choice is stated in the ADR-013 amendment: the
/// baseline may sit up to one sample period before the window, so a
/// delta can include activity that preceded the step. At sample
/// periods comparable to step durations neither convention is exact;
/// this one fails toward over-attribution, which is visible in the
/// figures, rather than toward a systematic zero, which is not.
fn bracket<T>(samples: &[T], elapsed: impl Fn(&T) -> u64, s: u64, e: u64) -> Option<(&T, &T)> {
    let lo = samples.partition_point(|x| elapsed(x) < s);
    let hi = samples.partition_point(|x| elapsed(x) <= e);
    if hi > lo {
        let base = if lo > 0 { lo - 1 } else { lo };
        Some((&samples[base], &samples[hi - 1]))
    } else {
        None
    }
}

/// Derives [`TrajectoryMetrics`] from a report and driver-side step
/// records (ADR-013).
///
/// Withholding, in the ADR-011/012 discipline — returns `None` when:
/// the report lacks `reference_instant_unix_ns` (pre-ADR-013 report,
/// no join possible); the report has no GPU timeline or fewer than
/// two samples (no energy basis); any KV-cache or phase counter
/// regresses within the run.
///
/// Individual steps are dropped (and named in `dropped_steps`) when
/// their window falls outside the run window or overlaps the
/// preceding kept step.
pub fn derive_trajectory(
    report: &crate::metrics::Report,
    steps: &[StepRecord],
) -> Option<TrajectoryMetrics> {
    derive_trajectory_from_timelines(
        report.reference_instant_unix_ns,
        report.gpu_timeline.as_ref(),
        report.kvcache_timeline.as_ref(),
        report.phase_timeline.as_ref(),
        steps,
    )
}

/// Timeline-level core of [`derive_trajectory`], for callers that
/// hold the raw timelines without a full [`crate::metrics::Report`]
/// — the `--sample-only` path (ADR-009), which is the attach-mode
/// symmetry the ADR-013 validation run relies on.
pub fn derive_trajectory_from_timelines(
    reference_instant_unix_ns: Option<u64>,
    gpu_timeline: Option<&is_core::GpuTimeline>,
    kvcache_timeline: Option<&is_core::KvCacheTimeline>,
    phase_timeline: Option<&is_core::PhaseTimeline>,
    steps: &[StepRecord],
) -> Option<TrajectoryMetrics> {
    let anchor = reference_instant_unix_ns?;
    let gpu = gpu_timeline?;
    if gpu.samples.len() < 2 {
        return None;
    }
    // Counter-regression withholding (ADR-011/012 discipline): a
    // regressed counter poisons every window delta, so the whole
    // derived layer is withheld, not patched around.
    if let Some(kv) = kvcache_timeline {
        if kv
            .samples
            .windows(2)
            .any(|p| p[1].hits < p[0].hits || p[1].queries < p[0].queries)
        {
            return None;
        }
    }
    if let Some(ph) = phase_timeline {
        // The timing legs are `Option` (ADR-014 D3), so `Option` ordering
        // must not stand in for numeric comparison: `None < Some(0)` would
        // read a capability gap as a regression. Compare the values only
        // when both endpoints carry them, and treat a pair that gains or
        // loses the family as the discontinuity it is.
        let regressed = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => b < a,
            (None, None) => false,
            _ => true,
        };
        if ph.samples.windows(2).any(|p| {
            p[1].prompt_tokens < p[0].prompt_tokens
                || p[1].generation_tokens < p[0].generation_tokens
                || regressed(p[0].prefill_ns, p[1].prefill_ns)
                || regressed(p[0].decode_ns, p[1].decode_ns)
        }) {
            return None;
        }
    }
    let run_start = gpu.samples.first().map(|s| s.elapsed_ns)?;
    let run_end = gpu.samples.last().map(|s| s.elapsed_ns)?;

    // Per-device sample views: segments only exist between
    // consecutive samples of the same device (the timeline is
    // interleaved by device within each tick).
    let mut per_device: std::collections::BTreeMap<u32, Vec<&is_core::GpuSample>> =
        std::collections::BTreeMap::new();
    for s in &gpu.samples {
        per_device.entry(s.device_index).or_default().push(s);
    }

    // Place steps in Report coordinates; drop out-of-run and
    // overlapping windows. Steps are judged in start order; a step
    // whose start precedes the previous kept step's end would
    // double-count energy and is dropped.
    let mut ordered: Vec<&StepRecord> = steps.iter().collect();
    ordered.sort_by_key(|s| (s.t_start_unix_ns, s.step_id));
    let mut kept: Vec<(&StepRecord, u64, u64)> = Vec::new();
    let mut dropped_steps: Vec<DroppedStep> = Vec::new();
    let mut last_end: Option<u64> = None;
    for s in ordered {
        let rel = s
            .t_start_unix_ns
            .checked_sub(anchor)
            .zip(s.t_end_unix_ns.checked_sub(anchor));
        let (start_rel, end_rel) = match rel {
            Some(w) => w,
            None => {
                // Starts before the anchor: outside the run by
                // construction.
                dropped_steps.push(DroppedStep {
                    step_id: s.step_id,
                    reason: DropReason::OutsideRunWindow,
                });
                continue;
            }
        };
        if start_rel < run_start || end_rel > run_end {
            dropped_steps.push(DroppedStep {
                step_id: s.step_id,
                reason: DropReason::OutsideRunWindow,
            });
            continue;
        }
        if let Some(prev_end) = last_end {
            if start_rel < prev_end {
                dropped_steps.push(DroppedStep {
                    step_id: s.step_id,
                    reason: DropReason::OverlapsPrecedingStep,
                });
                continue;
            }
        }
        last_end = Some(end_rel);
        kept.push((s, start_rel, end_rel));
    }

    let step_metrics: Vec<StepMetrics> = kept
        .iter()
        .map(|(rec, start, end)| {
            let step_area2: u128 = per_device
                .values()
                .map(|v| area2_mw_ns(v, Some((*start, *end))))
                .sum();
            let energy_mj = (step_area2 / 2_000_000_000) as u64;
            let samples_in_window = (gpu.samples.partition_point(|x| x.elapsed_ns <= *end)
                - gpu.samples.partition_point(|x| x.elapsed_ns < *start))
                as u64;
            let kv_deltas = kvcache_timeline
                .and_then(|t| bracket(&t.samples, |x| x.elapsed_ns, *start, *end))
                .map(|(f, l)| (l.hits - f.hits, l.queries - f.queries));
            let cache_hits_delta = kv_deltas.map(|(h, _)| h);
            let cache_queries_delta = kv_deltas.map(|(_, q)| q);
            let phase_deltas = phase_timeline
                .and_then(|t| bracket(&t.samples, |x| x.elapsed_ns, *start, *end))
                .map(|(f, l)| {
                    (
                        l.generation_tokens - f.generation_tokens,
                        l.prompt_tokens - f.prompt_tokens,
                    )
                });
            let generation_tokens_delta = phase_deltas.map(|(g, _)| g);
            let prompt_tokens_delta = phase_deltas.map(|(_, p)| p);
            let tokens_per_joule = match (rec.kind, generation_tokens_delta, energy_mj) {
                (StepKind::Tool, _, _) | (_, None, _) | (_, Some(0), _) | (_, _, 0) => None,
                (StepKind::LlmCall, Some(tokens), mj) => Some(tokens as f64 / (mj as f64 / 1000.0)),
            };
            let cache_hit_rate = match (cache_hits_delta, cache_queries_delta) {
                (Some(h), Some(q)) if q > 0 => Some(h as f64 / q as f64),
                _ => None,
            };
            StepMetrics {
                step_id: rec.step_id,
                kind: rec.kind,
                start_elapsed_ns: *start,
                end_elapsed_ns: *end,
                samples_in_window,
                energy_mj,
                generation_tokens_delta,
                prompt_tokens_delta,
                cache_hits_delta,
                cache_queries_delta,
                tokens_per_joule,
                cache_hit_rate,
            }
        })
        .collect();

    let total_area2: u128 = per_device.values().map(|v| area2_mw_ns(v, None)).sum();
    let total_energy_mj = (total_area2 / 2_000_000_000) as u64;
    let attributed_mj: u64 = step_metrics.iter().map(|s| s.energy_mj).sum();
    let llm_energy_mj: u64 = step_metrics
        .iter()
        .filter(|s| s.kind == StepKind::LlmCall)
        .map(|s| s.energy_mj)
        .sum();
    let tool_energy_mj: u64 = step_metrics
        .iter()
        .filter(|s| s.kind == StepKind::Tool)
        .map(|s| s.energy_mj)
        .sum();
    // Safe by construction: kept windows are disjoint, so the step
    // segments are a subset of the run segments, and integer floors
    // preserve Σ steps ≤ total. saturating_sub documents the intent;
    // the reconciliation itself is asserted in tests.
    let unattributed_energy_mj = total_energy_mj.saturating_sub(attributed_mj);
    let total_generation_tokens = phase_timeline
        .and_then(|t| match (t.samples.first(), t.samples.last()) {
            (Some(f), Some(l)) => Some(l.generation_tokens - f.generation_tokens),
            _ => None,
        })
        .unwrap_or(0);
    let trajectory_tokens_per_joule = if total_generation_tokens == 0 || total_energy_mj == 0 {
        None
    } else {
        Some(total_generation_tokens as f64 / (total_energy_mj as f64 / 1000.0))
    };

    Some(TrajectoryMetrics {
        steps: step_metrics,
        total_energy_mj,
        total_generation_tokens,
        trajectory_tokens_per_joule,
        llm_energy_mj,
        tool_energy_mj,
        unattributed_energy_mj,
        dropped_steps,
    })
}

#[cfg(test)]
mod derive_tests {
    use super::*;
    use crate::metrics::{Report, TimingMetrics};
    use is_core::{
        GpuSample, GpuTimeline, KvCacheSample, KvCacheTimeline, PhaseSample, PhaseTimeline,
        RequestTiming,
    };

    /// Wall-clock anchor of the synthetic run.
    const ANCHOR: u64 = 1_000_000_000_000_000_000;
    /// One second in nanoseconds; also the sample period.
    const S: u64 = 1_000_000_000;

    fn gpu_sample(elapsed_ns: u64, device_index: u32, power_mw: u32) -> GpuSample {
        GpuSample {
            elapsed_ns,
            device_index,
            memory_used_bytes: 1024,
            memory_total_bytes: 2048,
            utilization_percent: 50,
            temperature_celsius: 60,
            power_draw_milliwatts: power_mw,
        }
    }

    /// 5 ticks (0..=4 s), 2 devices at constant 100 W and 50 W.
    /// Each 1 s segment carries exactly 150_000 mJ across devices;
    /// the whole run carries 600_000 mJ. KV counters advance by
    /// 10 hits / 20 queries per tick; phase counters by 100
    /// generation / 50 prompt tokens per tick.
    fn synthetic_report() -> Report {
        let mut gpu = GpuTimeline::new(S);
        for tick in 0..5u64 {
            gpu.push(gpu_sample(tick * S, 0, 100_000));
            gpu.push(gpu_sample(tick * S, 1, 50_000));
        }
        let kv = KvCacheTimeline {
            accounting: Some(is_core::HitRateAccounting::BlockAligned),
            samples: (0..5u64)
                .map(|t| KvCacheSample {
                    elapsed_ns: t * S,
                    hits: t * 10,
                    queries: t * 20,
                })
                .collect(),
            sample_period_ns: S,
        };
        let phase = PhaseTimeline {
            samples: (0..5u64)
                .map(|t| PhaseSample {
                    elapsed_ns: t * S,
                    prompt_tokens: t * 50,
                    generation_tokens: t * 100,
                    prefill_ns: Some(t * 1_000),
                    decode_ns: Some(t * 2_000),
                })
                .collect(),
            sample_period_ns: S,
        };
        Report {
            reference_instant_unix_ns: Some(ANCHOR),
            request_timing: RequestTiming::new(vec![], 0),
            resource_timeline: None,
            gpu_timeline: Some(gpu),
            timing: TimingMetrics {
                token_count: 0,
                ttft_ns: None,
                total_latency_ns: 0,
                tokens_per_second: None,
                inter_token_latency: None,
            },
            resource: None,
            gpu: None,
            efficiency: None,
            kvcache_timeline: Some(kv),
            kvcache: None,
            phase_timeline: Some(phase),
            phase_energy: None,
            trajectory: None,
        }
    }

    fn step(step_id: u64, kind: StepKind, start_ns: u64, end_ns: u64) -> StepRecord {
        StepRecord {
            step_id,
            kind,
            t_start_unix_ns: ANCHOR + start_ns,
            t_end_unix_ns: ANCHOR + end_ns,
        }
    }

    #[test]
    fn happy_path_two_llm_one_tool() {
        let report = synthetic_report();
        let steps = vec![
            step(1, StepKind::LlmCall, 0, S),
            step(2, StepKind::Tool, 2 * S, 3 * S),
            step(3, StepKind::LlmCall, 3 * S, 4 * S),
        ];
        let t = derive_trajectory(&report, &steps).expect("valid inputs derive");
        assert_eq!(t.steps.len(), 3);
        assert!(t.dropped_steps.is_empty());

        let s1 = &t.steps[0];
        assert_eq!(s1.energy_mj, 150_000);
        assert_eq!(s1.samples_in_window, 4); // 2 ticks x 2 devices
        assert_eq!(s1.cache_hits_delta, Some(10));
        assert_eq!(s1.cache_queries_delta, Some(20));
        assert_eq!(s1.cache_hit_rate, Some(0.5));
        assert_eq!(s1.generation_tokens_delta, Some(100));
        assert_eq!(s1.prompt_tokens_delta, Some(50));
        // 100 tokens / 150 J
        let tpj = s1.tokens_per_joule.expect("llm step with tokens");
        assert!((tpj - 100.0 / 150.0).abs() < 1e-12);

        let s2 = &t.steps[1];
        assert_eq!(s2.kind, StepKind::Tool);
        assert_eq!(s2.energy_mj, 150_000);
        // Tokens flow in the engine during the tool window (synthetic
        // counters advance every tick), but a tool step's efficiency
        // is absence, not a ratio.
        assert_eq!(s2.tokens_per_joule, None);

        assert_eq!(t.total_energy_mj, 600_000);
        assert_eq!(t.llm_energy_mj, 300_000);
        assert_eq!(t.tool_energy_mj, 150_000);
        assert_eq!(t.unattributed_energy_mj, 150_000); // gap [1s, 2s]
        assert_eq!(t.total_generation_tokens, 400);
        let ttpj = t.trajectory_tokens_per_joule.expect("tokens and energy");
        assert!((ttpj - 400.0 / 600.0).abs() < 1e-12);
    }

    #[test]
    fn reconciliation_steps_plus_unattributed_equals_total() {
        let report = synthetic_report();
        let steps = vec![
            step(1, StepKind::LlmCall, 0, S),
            step(2, StepKind::Tool, 2 * S, 3 * S),
        ];
        let t = derive_trajectory(&report, &steps).expect("derives");
        assert_eq!(
            t.llm_energy_mj + t.tool_energy_mj + t.unattributed_energy_mj,
            t.total_energy_mj,
            "steps plus unattributed must reconcile exactly to whole-run energy"
        );
    }

    #[test]
    fn boundary_straddling_segments_land_in_unattributed() {
        let report = synthetic_report();
        // Window [0.5s, 1.5s]: no inter-sample segment has both
        // endpoints inside it, so the step's energy is 0 and the
        // straddled segments belong to the unattributed remainder.
        let steps = vec![step(1, StepKind::LlmCall, S / 2, S + S / 2)];
        let t = derive_trajectory(&report, &steps).expect("derives");
        assert_eq!(t.steps[0].energy_mj, 0);
        assert_eq!(t.steps[0].samples_in_window, 2); // tick at 1s, both devices
        assert_eq!(t.unattributed_energy_mj, t.total_energy_mj);
        assert_eq!(
            t.llm_energy_mj + t.tool_energy_mj + t.unattributed_energy_mj,
            t.total_energy_mj
        );
    }

    #[test]
    fn sub_period_step_declares_grid_resolution() {
        let report = synthetic_report();
        // Window around a single tick: one bracketing sample per
        // device, zero-width counter window, no interpolation.
        let steps = vec![step(1, StepKind::LlmCall, S - S / 10, S + S / 10)];
        let t = derive_trajectory(&report, &steps).expect("derives");
        let s1 = &t.steps[0];
        assert_eq!(s1.samples_in_window, 2);
        assert_eq!(s1.energy_mj, 0);
        // Baseline is t=0, end is t=1s: the window now carries the
        // tick's real increment instead of a false zero.
        assert_eq!(s1.cache_hits_delta, Some(10));
        assert_eq!(s1.generation_tokens_delta, Some(100));
        assert_eq!(s1.tokens_per_joule, None); // zero energy is absence
    }

    #[test]
    fn withheld_without_anchor() {
        let mut report = synthetic_report();
        report.reference_instant_unix_ns = None;
        let steps = vec![step(1, StepKind::LlmCall, 0, S)];
        assert_eq!(derive_trajectory(&report, &steps), None);
    }

    #[test]
    fn withheld_without_gpu_timeline_or_enough_samples() {
        let mut report = synthetic_report();
        report.gpu_timeline = None;
        let steps = vec![step(1, StepKind::LlmCall, 0, S)];
        assert_eq!(derive_trajectory(&report, &steps), None);

        let mut report = synthetic_report();
        let mut gpu = GpuTimeline::new(S);
        gpu.push(gpu_sample(0, 0, 100_000));
        report.gpu_timeline = Some(gpu);
        assert_eq!(derive_trajectory(&report, &steps), None);
    }

    #[test]
    fn withheld_on_kv_counter_regression() {
        let mut report = synthetic_report();
        report.kvcache_timeline.as_mut().unwrap().samples[3].queries = 0;
        let steps = vec![step(1, StepKind::LlmCall, 0, S)];
        assert_eq!(derive_trajectory(&report, &steps), None);
    }

    #[test]
    fn withheld_on_phase_counter_regression() {
        let mut report = synthetic_report();
        report.phase_timeline.as_mut().unwrap().samples[2].generation_tokens = 0;
        let steps = vec![step(1, StepKind::LlmCall, 0, S)];
        assert_eq!(derive_trajectory(&report, &steps), None);
    }

    #[test]
    fn steps_outside_run_window_are_dropped_with_reason() {
        let report = synthetic_report();
        let steps = vec![
            // Starts before the anchor (wall-clock precedes run start).
            StepRecord {
                step_id: 1,
                kind: StepKind::LlmCall,
                t_start_unix_ns: ANCHOR - S,
                t_end_unix_ns: ANCHOR + S,
            },
            // Ends beyond the last GPU sample.
            step(2, StepKind::LlmCall, 3 * S, 5 * S),
            // In-window control.
            step(3, StepKind::LlmCall, 0, S),
        ];
        let t = derive_trajectory(&report, &steps).expect("derives");
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].step_id, 3);
        assert_eq!(t.dropped_steps.len(), 2);
        assert!(t.dropped_steps.contains(&DroppedStep {
            step_id: 1,
            reason: DropReason::OutsideRunWindow,
        }));
        assert!(t.dropped_steps.contains(&DroppedStep {
            step_id: 2,
            reason: DropReason::OutsideRunWindow,
        }));
    }

    #[test]
    fn overlapping_step_is_dropped_and_never_double_counted() {
        let report = synthetic_report();
        let steps = vec![
            step(1, StepKind::LlmCall, 0, 2 * S),
            step(2, StepKind::LlmCall, S, 3 * S), // overlaps step 1
        ];
        let t = derive_trajectory(&report, &steps).expect("derives");
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.steps[0].step_id, 1);
        assert_eq!(
            t.dropped_steps,
            vec![DroppedStep {
                step_id: 2,
                reason: DropReason::OverlapsPrecedingStep,
            }]
        );
        // The kept step covers [0, 2s] = 300_000 mJ; nothing from the
        // overlapping window is counted twice.
        assert_eq!(t.llm_energy_mj, 300_000);
        assert_eq!(
            t.llm_energy_mj + t.tool_energy_mj + t.unattributed_energy_mj,
            t.total_energy_mj
        );
    }

    /// A window that opens between two samples must count the
    /// increment that happened before its first interior sample.
    /// Baseline is the last sample at or before the window start, not
    /// the first sample inside it: counters that jump at the start of
    /// a step (prompt tokens on prefill) otherwise read as zero, and
    /// progressive counters under-report. Reproduces the A10 vLLM
    /// evidence where per-step prompt deltas were 0 while the
    /// timeline showed the prefill jumps.
    #[test]
    fn window_opening_between_samples_counts_the_pre_window_increment() {
        let report = synthetic_report();
        // Window 1.5s -> 3.0s. Interior samples: t=2s, t=3s.
        // Baseline must be t=1s (gen=100, prompt=50, hits=10, q=20);
        // last interior t=3s (gen=300, prompt=150, hits=30, q=60).
        let steps = vec![step(1, StepKind::LlmCall, 3 * S / 2, 3 * S)];
        let t = derive_trajectory(&report, &steps).expect("valid inputs derive");
        let s1 = &t.steps[0];
        assert_eq!(s1.generation_tokens_delta, Some(200));
        assert_eq!(s1.prompt_tokens_delta, Some(100));
        assert_eq!(s1.cache_hits_delta, Some(20));
        assert_eq!(s1.cache_queries_delta, Some(40));
    }

    #[test]
    fn missing_kv_and_phase_timelines_yield_absent_deltas_not_withholding() {
        let mut report = synthetic_report();
        report.kvcache_timeline = None;
        report.phase_timeline = None;
        let steps = vec![step(1, StepKind::LlmCall, 0, S)];
        let t = derive_trajectory(&report, &steps).expect("derives on GPU alone");
        assert_eq!(t.steps[0].cache_hits_delta, None);
        assert_eq!(t.steps[0].cache_hit_rate, None);
        assert_eq!(t.steps[0].generation_tokens_delta, None);
        assert_eq!(t.steps[0].tokens_per_joule, None);
        assert_eq!(t.total_generation_tokens, 0);
        assert_eq!(t.trajectory_tokens_per_joule, None);
    }
}
