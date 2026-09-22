//! Integration test for the LoRA read in `--sample-only` (ADR-017).
//!
//! Invokes the compiled binary, in the shape of `tests/sample_only_kv.rs`,
//! against a `/metrics` endpoint served from a thread here.
//!
//! The body is the `vllm:lora_requests_info` family, headers and both
//! series, extracted verbatim from
//! `crates/is-metrics/tests/fixtures/llm-d-inference-sim-e924683-lora-unique-latest.txt`,
//! a live capture taken with a request in flight: one series strictly the
//! most recent. As in `sample_only_kv.rs`, it is a partial, static copy.
//! Fidelity to the producer is covered by the `is-metrics` tests, which read
//! the whole capture; what is covered here is the wiring.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

const MODEL: &str = "Qwen/Qwen2.5-7B-Instruct";

const LORA: &str = r#"# HELP vllm:lora_requests_info Running stats on lora requests.
# TYPE vllm:lora_requests_info gauge
vllm:lora_requests_info{max_lora="8",running_lora_adapters="",waiting_lora_adapters=""} 1.790060019e+09
vllm:lora_requests_info{max_lora="8",running_lora_adapters="a1",waiting_lora_adapters=""} 1.790060021e+09
"#;

/// Serves `body` to every request. Returns the endpoint URL; the thread
/// ends with the process.
fn serving(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("read the bound address");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}/metrics")
}

/// Runs `--sample-only` for one second with `extra` arguments and returns
/// the report it prints.
fn run(extra: &[&str]) -> serde_json::Value {
    let pid = std::process::id().to_string();
    let mut args = vec![
        "--sample-only",
        "--pid",
        pid.as_str(),
        "--duration-secs",
        "1",
    ];
    args.extend_from_slice(extra);
    let output = Command::new(env!("CARGO_BIN_EXE_inferscope"))
        .args(&args)
        .output()
        .expect("binary runs");
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "stdout: {text}\nstderr: {stderr}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{text}"))
}

/// The read reaches the report, on the path a campaign runs.
///
/// Before the read was wired in, the report had no `lora` key whatever the
/// endpoint served: on that code this test fails on a missing key.
#[test]
fn sample_only_records_the_most_recent_adapter_set() {
    let endpoint = serving(LORA);
    let report = run(&[
        "--metrics-endpoint",
        &endpoint,
        "--model",
        MODEL,
        "--engine",
        "vllm",
    ]);
    let series = &report["lora"]["latest"]["series"];
    assert_eq!(
        series["running_lora_adapters"],
        serde_json::json!(["a1"]),
        "{report}"
    );
    assert_eq!(series["max_lora"], 8, "{report}");
    assert_eq!(report["schema_version"], 2, "{report}");
}

/// With no metrics endpoint nothing is read and the field is left out. At
/// schema version 2 that absence resolves as not recorded.
#[test]
fn without_an_endpoint_the_field_is_absent() {
    let report = run(&[]);
    assert!(report.get("lora").is_none(), "{report}");
    assert_eq!(report["schema_version"], 2, "{report}");
}

/// An endpoint that refuses the connection is a failed read, recorded as
/// one rather than as an empty set.
#[test]
fn an_unreachable_endpoint_records_a_failed_read() {
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("read the bound address")
        .port();
    let endpoint = format!("http://127.0.0.1:{port}/metrics");
    let report = run(&[
        "--metrics-endpoint",
        &endpoint,
        "--model",
        MODEL,
        "--engine",
        "vllm",
    ]);
    assert_eq!(report["lora"], "read_failed", "{report}");
}
