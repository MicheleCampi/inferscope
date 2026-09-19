//! Integration test for the KV-cache scrape in `--sample-only`.
//!
//! Invokes the compiled binary via `CARGO_BIN_EXE_inferscope`, in the
//! shape of `tests/pid_check.rs`, against a `/metrics` endpoint served
//! from a thread here.
//!
//! The endpoint is a plain `TcpListener`, with no new dependency:
//! what this test needs is two different bodies inside one sampling
//! window, so that the window carries a counter delta.
//!
//! The exposition is copied from
//! `crates/is-metrics/tests/fixtures/vllm-prometheus-client-exposition.txt`
//! at revision 994ab35: the two prefix-cache counters and their
//! headers, with the second body's values advanced. It is a partial,
//! static copy, and does not follow that fixture. Format
//! fidelity to a real vLLM endpoint is covered by the `is-metrics`
//! tests, which read the canonical fixture; what is covered here is
//! the wiring.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

const MODEL: &str = "Qwen/Qwen2.5-7B-Instruct";

const COLD: &str = r#"# HELP vllm:prefix_cache_hits_total Prefix cache hits.
# TYPE vllm:prefix_cache_hits_total counter
vllm:prefix_cache_hits_total{model_name="Qwen/Qwen2.5-7B-Instruct"} 144.0
# HELP vllm:prefix_cache_queries_total Prefix cache queries.
# TYPE vllm:prefix_cache_queries_total counter
vllm:prefix_cache_queries_total{model_name="Qwen/Qwen2.5-7B-Instruct"} 270.0
"#;

const WARM: &str = r#"# HELP vllm:prefix_cache_hits_total Prefix cache hits.
# TYPE vllm:prefix_cache_hits_total counter
vllm:prefix_cache_hits_total{model_name="Qwen/Qwen2.5-7B-Instruct"} 200.0
# HELP vllm:prefix_cache_queries_total Prefix cache queries.
# TYPE vllm:prefix_cache_queries_total counter
vllm:prefix_cache_queries_total{model_name="Qwen/Qwen2.5-7B-Instruct"} 400.0
"#;

/// Serves `COLD` for the first 500 ms and `WARM` after that,
/// so the scrape window carries a non-zero counter delta. Returns the
/// bound address; the thread ends with the process.
fn warming_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("read the bound address");
    let opened = std::time::Instant::now();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            // Switch on elapsed time, not on a request count: the
            // client may open more connections than it makes scrapes,
            // and counting them made this test pass or fail by luck.
            let body = if opened.elapsed() < std::time::Duration::from_millis(500) {
                COLD
            } else {
                WARM
            };
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

/// The KV-cache scrape reaches the `--sample-only` report.
///
/// Before the scrape was wired into this path the report carried no
/// KV-cache section at all, however many scrapes the endpoint served:
/// on that code this test fails on a missing key, not a wrong value.
#[test]
fn sample_only_emits_the_kv_cache_section() {
    let endpoint = warming_endpoint();
    let output = Command::new(env!("CARGO_BIN_EXE_inferscope"))
        .args([
            "--sample-only",
            "--pid",
            &std::process::id().to_string(),
            "--duration-secs",
            "1",
            "--metrics-endpoint",
            &endpoint,
            "--model",
            MODEL,
            "--engine",
            "vllm",
            "--metrics-period-ms",
            "200",
        ])
        .output()
        .expect("binary runs");
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "stdout: {text}\nstderr: {stderr}");

    let report: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("stdout is not JSON: {e}\n{text}"));

    let samples = report["kvcache_timeline"]["samples"]
        .as_array()
        .unwrap_or_else(|| panic!("no kvcache_timeline samples in the report\n{text}"));
    assert!(
        samples.len() >= 2,
        "a 1 s window at 200 ms should hold at least two scrapes, got {}\n{text}",
        samples.len()
    );
    assert_eq!(samples[0]["hits"], 144, "first scrape reads the cold cache");
    assert_eq!(
        samples[samples.len() - 1]["hits"],
        200,
        "the last scrape reads the warmed cache"
    );

    // 200 - 144 hits over 400 - 270 queries.
    let kvcache = &report["kvcache"];
    assert_eq!(kvcache["hits_delta"], 56, "window hits delta\n{text}");
    assert_eq!(
        kvcache["queries_delta"], 130,
        "window queries delta\n{text}"
    );
}
