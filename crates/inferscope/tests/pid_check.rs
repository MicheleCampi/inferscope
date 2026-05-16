//! Integration tests for the orchestrator's pre-flight PID check.
//!
//! These tests invoke the compiled `inferscope` binary directly
//! via the `CARGO_BIN_EXE_inferscope` env var that Cargo exposes
//! to integration tests in the same package.

use std::process::Command;

/// A PID that cannot plausibly belong to a live process. Linux
/// default `pid_max` is 2^22 (4194304) on 64-bit; 9_999_999 is
/// safely above that ceiling on any realistic system.
const NONEXISTENT_PID: &str = "9999999";

#[test]
fn fails_fast_when_pid_does_not_exist() {
    let bin = env!("CARGO_BIN_EXE_inferscope");

    // Endpoint points at a port we don't bind. If the PID check
    // failed to short-circuit, the probe would try to connect
    // here and we'd see a network-shaped error message instead
    // of the PID-shaped one.
    let output = Command::new(bin)
        .args([
            "--endpoint",
            "http://127.0.0.1:1",
            "--model",
            "irrelevant",
            "--prompt",
            "irrelevant",
            "--max-tokens",
            "1",
            "--pid",
            NONEXISTENT_PID,
        ])
        .output()
        .expect("failed to invoke the inferscope binary");

    assert!(
        !output.status.success(),
        "expected non-zero exit when --pid is invalid, got success"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("pid {NONEXISTENT_PID} does not exist")),
        "stderr did not mention the missing pid; got: {stderr}"
    );
    // Negative check: the probe must not have run. If it had,
    // we'd see a connection-refused / network error in stderr.
    assert!(
        !stderr.contains("probe failed"),
        "probe was invoked despite invalid pid; stderr: {stderr}"
    );
}
