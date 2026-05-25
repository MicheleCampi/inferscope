//! OpenTelemetry export of inferscope reports.
//!
//! This module is gated behind the `otel-export` Cargo feature. When
//! enabled, [`export_to_otel`] takes a derived [`crate::Report`] and
//! emits it as a single OTLP/HTTP trace: one root span representing
//! the inference run, with the per-token arrivals attached as span
//! events and the derived aggregates attached as span attributes.
//!
//! Design choices recorded in `docs/adr/008-opentelemetry-export.md`:
//!
//! - **One root span per run.** The run is the unit of work; events
//!   on the root span carry per-token timing.
//! - **Token arrivals as events, not child spans.** A token arrival
//!   is a timestamp, not a sub-operation with duration. Span events
//!   are the OTel-semantic-correct shape.
//! - **GPU and resource aggregates as attributes.** Lossless summary
//!   without overwhelming the trace with timeline samples.
//! - **OTLP over HTTP/protobuf, not gRPC.** Smaller dependency
//!   footprint, traverses corporate firewalls without special config.
//! - **Export failure does not fail the run.** Observability is
//!   secondary to the profiling result. Errors surface via
//!   `Result<(), OtelExportError>` for the caller to log.

use std::time::{Duration, SystemTime};

use opentelemetry::trace::{Span, SpanKind, Tracer, TracerProvider as _};
use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use thiserror::Error;

use crate::Report;

/// Errors that can occur while exporting a report to OpenTelemetry.
///
/// Construction is split by failure phase so a caller can distinguish
/// configuration errors (likely user-recoverable) from transport errors
/// (likely transient or out of the caller's control).
#[derive(Debug, Error)]
pub enum OtelExportError {
    /// Setting up the tracer provider failed. Typically a malformed
    /// endpoint URL or a missing required environment variable.
    #[error("failed to initialise OTLP exporter: {0}")]
    SetupFailed(String),

    /// The exporter could not flush the span to the configured
    /// endpoint. Common causes: collector unreachable, network
    /// timeout, collector rejected the payload.
    #[error("failed to export span to {endpoint}: {message}")]
    ExportFailed { endpoint: String, message: String },
}

/// Exports a derived [`Report`] as an OpenTelemetry trace to the
/// configured OTLP/HTTP endpoint.
///
/// `endpoint` is expected to be the **base URL** of the OTLP receiver,
/// not the full traces path. For a local OpenTelemetry Collector with
/// default settings this is `http://localhost:4318`. The function
/// appends `/v1/traces` internally per OTLP/HTTP spec.
///
/// On success returns `Ok(())`. On failure returns one of the
/// variants of [`OtelExportError`]. The caller is expected to log the
/// error and continue; export failure should never fail the calling
/// run.
///
/// Behaviour:
///
/// - A single root span `inferscope.run` is created and immediately
///   ended; its duration matches `report.timing.total_latency_ns`.
/// - All derived metrics are attached as span attributes.
/// - Each token arrival in `report.request_timing.tokens` is
///   attached as a span event named `token.arrival`, with its
///   timestamp set to `run_start + elapsed_ns` so trace UIs render
///   the events in temporal order.
/// - The provider is shut down before returning, which forces a
///   synchronous flush of all spans to the collector.
pub fn export_to_otel(report: &Report, endpoint: &str) -> Result<(), OtelExportError> {
    // Build the OTLP/HTTP exporter pointed at the caller's endpoint.
    // The opentelemetry-otlp 0.32 builder appends /v1/traces itself
    // when given the base URL, matching the OTLP/HTTP spec.
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_protocol(Protocol::HttpBinary)
        .with_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| OtelExportError::SetupFailed(e.to_string()))?;

    // Build resource attributes that apply to every span we emit.
    // These map to OTel semantic conventions where possible.
    let resource = Resource::builder()
        .with_attribute(KeyValue::new("service.name", "inferscope"))
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();

    // Tracer provider with a simple (synchronous) span processor.
    // SimpleSpanProcessor flushes on every span end, which suits our
    // one-shot CLI shape better than batching.
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("inferscope");

    // Compute timestamps before opening the span so the recorded
    // start matches the run's actual wall-clock start as closely as
    // possible. We do not have the original run start time on a
    // Report (it is a pure data type), so we anchor on SystemTime::now()
    // minus total_latency_ns. The event offsets remain accurate
    // relative to that anchor.
    let now = SystemTime::now();
    let run_duration = Duration::from_nanos(report.timing.total_latency_ns);
    let run_start = now.checked_sub(run_duration).unwrap_or(now);

    // Open the root span with the synthesised start time, then attach
    // attributes and events, then end with the matching end time.
    let mut span = tracer
        .span_builder("inferscope.run")
        .with_kind(SpanKind::Client)
        .with_start_time(run_start)
        .start(&tracer);

    // Endpoint and timing attributes.
    span.set_attribute(KeyValue::new("inferscope.endpoint", endpoint.to_string()));
    span.set_attribute(KeyValue::new(
        "inferscope.timing.token_count",
        report.timing.token_count as i64,
    ));
    span.set_attribute(KeyValue::new(
        "inferscope.timing.total_latency_ns",
        report.timing.total_latency_ns as i64,
    ));
    if let Some(ttft) = report.timing.ttft_ns {
        span.set_attribute(KeyValue::new("inferscope.timing.ttft_ns", ttft as i64));
    }
    if let Some(rate) = report.timing.tokens_per_second {
        span.set_attribute(KeyValue::new("inferscope.timing.tokens_per_second", rate));
    }
    if let Some(dist) = &report.timing.inter_token_latency {
        span.set_attribute(KeyValue::new(
            "inferscope.timing.inter_token_p50_ns",
            dist.p50_ns as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.timing.inter_token_p99_ns",
            dist.p99_ns as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.timing.inter_token_max_ns",
            dist.max_ns as i64,
        ));
    }

    // Resource attributes if a resource timeline was captured.
    if let Some(res) = &report.resource {
        span.set_attribute(KeyValue::new(
            "inferscope.resource.rss_max_bytes",
            res.rss_max_bytes as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.resource.rss_mean_bytes",
            res.rss_mean_bytes as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.resource.thread_max",
            res.thread_max as i64,
        ));
        if let Some(cpu) = res.cpu_mean_percent {
            span.set_attribute(KeyValue::new("inferscope.resource.cpu_mean_percent", cpu));
        }
    }

    // GPU attributes if a GPU timeline was captured.
    if let Some(gpu) = &report.gpu {
        span.set_attribute(KeyValue::new(
            "inferscope.gpu.device_count",
            gpu.device_count as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.gpu.memory_used_max_bytes",
            gpu.memory_used_max_bytes as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.gpu.utilization_mean_percent",
            gpu.utilization_mean_percent as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.gpu.power_mean_milliwatts",
            gpu.power_mean_milliwatts as i64,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.gpu.temperature_max_celsius",
            gpu.temperature_max_celsius as i64,
        ));
    }

    // One event per token arrival, timestamped at run_start + elapsed.
    for token in &report.request_timing.tokens {
        let token_time = run_start
            .checked_add(Duration::from_nanos(token.elapsed_ns))
            .unwrap_or(run_start);
        span.add_event_with_timestamp(
            "token.arrival",
            token_time,
            vec![
                KeyValue::new("token.index", token.index as i64),
                KeyValue::new("token.elapsed_ns", token.elapsed_ns as i64),
            ],
        );
    }

    // End the span at run_start + total_latency_ns, matching the
    // actual run duration rather than the wall-clock at function exit.
    let run_end = run_start.checked_add(run_duration).unwrap_or(now);
    span.end_with_timestamp(run_end);

    // Force a synchronous flush before returning. shutdown() blocks
    // until all pending spans have been delivered or the underlying
    // transport gives up; the global provider is replaced so a second
    // call to export_to_otel rebuilds from scratch.
    global::set_tracer_provider(provider.clone());
    provider
        .shutdown()
        .map_err(|e| OtelExportError::ExportFailed {
            endpoint: endpoint.to_string(),
            message: e.to_string(),
        })?;

    Ok(())
}
