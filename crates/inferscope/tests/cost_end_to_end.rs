//! End-to-end rehearsal of the cost chain, at zero cost.
//!
//! The GPU sampler cannot run on a CPU-only machine, so the timeline
//! is synthesised here and fed to the same derivation the orchestrator
//! uses. That leaves NVML itself unexercised — the gap that produced
//! both runbook failures of 2026-07-21 — but everything downstream of
//! a sample is the code the GPU session will run, including the
//! occupancy basis, which no stored report can exercise because none
//! carries a run duration.

use std::process::Command;

use is_core::{DeviceEnergy, EnergySource, GpuSample, GpuTimeline};
use is_report::{
    derive_trajectory_from_timelines, render_resource_json, ResourceReport, StepKind, StepRecord,
};

const ANCHOR: u64 = 1_700_000_000_000_000_000;
const TICK_NS: u64 = 100_000_000; // 100 ms
const WATTS: u32 = 200_000; // milliwatts

/// A timeline of constant draw, one device, `secs` seconds long.
///
/// Constant power makes the arithmetic checkable by hand: 200 W over
/// 60 s is 12 000 J, whatever the integration does internally.
fn timeline(secs: u64) -> GpuTimeline {
    let ticks = secs * 1_000_000_000 / TICK_NS;
    GpuTimeline {
        samples: (0..=ticks)
            .map(|i| GpuSample {
                elapsed_ns: i * TICK_NS,
                device_index: 0,
                memory_used_bytes: 8_000_000_000,
                memory_total_bytes: 24_000_000_000,
                utilization_percent: 90,
                temperature_celsius: 60,
                power_draw_milliwatts: WATTS,
            })
            .collect(),
        sample_period_ns: TICK_NS,
        // The GPU session will read a hardware counter (ADR-010
        // reports `source: counter` on A10 and H100), not integrate
        // power. Declare the same shape here: 200 W over `secs`.
        energy: Some(vec![DeviceEnergy {
            device_index: 0,
            energy_millijoules: (WATTS as u64) * secs,
            source: EnergySource::Counter,
        }]),
    }
}

fn step(id: u64, kind: StepKind, start_s: u64, end_s: u64) -> StepRecord {
    StepRecord {
        step_id: id,
        kind,
        t_start_unix_ns: ANCHOR + start_s * 1_000_000_000,
        t_end_unix_ns: ANCHOR + end_s * 1_000_000_000,
    }
}

fn write_report(steps: &[StepRecord], secs: u64) -> (tempfile::TempDir, std::path::PathBuf) {
    let gpu = timeline(secs);
    let trajectory = derive_trajectory_from_timelines(Some(ANCHOR), Some(&gpu), None, None, steps)
        .expect("synthetic timeline yields a trajectory");

    let report = ResourceReport {
        reference_instant_unix_ns: Some(ANCHOR),
        pid: 1,
        include_descendants: false,
        sample_period_ms: 100,
        duration_secs: secs,
        resource: None,
        gpu: None,
        phase_timeline: None,
        phase_energy: None,
        trajectory: Some(trajectory),
        schema_version: None,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    std::fs::write(&path, render_resource_json(&report).unwrap()).unwrap();
    (dir, path)
}

fn cost(path: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_inferscope"))
        .arg("cost")
        .arg("--report")
        .arg(path)
        .args(args)
        .output()
        .expect("binary runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn occupancy_prices_a_post_adr_015_report() {
    // 60 s run, 40 s of it inside step windows.
    let steps = [
        step(1, StepKind::LlmCall, 5, 25),
        step(2, StepKind::Tool, 25, 30),
        step(3, StepKind::LlmCall, 30, 45),
    ];
    let (_dir, path) = write_report(&steps, 60);

    let (ok, out) = cost(&path, &["--usd-per-hour", "3.60"]);
    assert!(ok, "{out}");
    // 60 s at $3.60/hour is exactly $0.06.
    assert!(out.contains("run window:       $0.060000"), "{out}");
    // 40 s attributed, 20 s residual: a third of the run.
    assert!(out.contains("$0.020000 (33.3% of run)"), "{out}");
    assert!(out.contains("$3.6000/hour"), "{out}");
}

#[test]
fn a_step_outside_the_window_is_dropped_rather_than_clipped() {
    // The failure mode the GPU session has to avoid: attaching late
    // or detaching early does not shorten the residual, it removes
    // whole steps from attribution and inflates it by their duration.
    let inside = [step(1, StepKind::LlmCall, 5, 25)];
    let straddling = [
        step(1, StepKind::LlmCall, 5, 25),
        step(2, StepKind::LlmCall, 50, 70), // ends past the 60 s window
    ];

    let (_d1, p1) = write_report(&inside, 60);
    let (_d2, p2) = write_report(&straddling, 60);

    let (ok1, out1) = cost(&p1, &["--usd-per-hour", "3.60"]);
    let (ok2, out2) = cost(&p2, &["--usd-per-hour", "3.60"]);
    assert!(ok1 && ok2, "{out1}{out2}");

    // Both price the same run window, and the straddling step
    // contributes nothing: it is absent, not truncated at 60 s.
    assert!(out1.contains("run window:       $0.060000"), "{out1}");
    assert!(out2.contains("run window:       $0.060000"), "{out2}");
    assert!(out1.contains("(66.7% of run)"), "{out1}");
    assert!(out2.contains("(66.7% of run)"), "{out2}");
    assert!(out2.contains("(1 steps"), "{out2}");
}
