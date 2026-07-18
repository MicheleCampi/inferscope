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
