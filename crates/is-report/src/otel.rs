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
/// **Runtime requirement.** This function constructs an HTTP client
/// via hyper and therefore must be called from within a tokio runtime
/// context (i.e. inside `Runtime::block_on` or a `#[tokio::main]`
/// function). The inferscope binary already satisfies this because
/// the orchestrator runs inside `Runtime::block_on`.
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
    // possible. When the report carries the ADR-013 anchor, the span
    // is placed at the run's true wall-clock start. On pre-ADR-013
    // reports (anchor absent) we fall back to the historical
    // behaviour: SystemTime::now() minus total_latency_ns, which
    // places spans at export time rather than run time. The event
    // offsets remain accurate relative to either anchor.
    let run_start = match report.reference_instant_unix_ns {
        Some(ns) => SystemTime::UNIX_EPOCH + Duration::from_nanos(ns),
        None => {
            let now = SystemTime::now();
            let run_duration = Duration::from_nanos(report.timing.total_latency_ns);
            now.checked_sub(run_duration).unwrap_or(now)
        }
    };

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

    // Energy-efficiency attributes (ADR-010). Exported as floats;
    // tokens_per_watt is omitted because it is identically equal to
    // tokens_per_joule (tokens / (W*s) = tokens / J), so emitting it
    // would invite double-counting in dashboards.
    if let Some(eff) = &report.efficiency {
        span.set_attribute(KeyValue::new(
            "inferscope.efficiency.energy_joules",
            eff.energy_joules,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.efficiency.energy_per_token_mj",
            eff.energy_per_token_mj,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.efficiency.tokens_per_joule",
            eff.tokens_per_joule,
        ));
        span.set_attribute(KeyValue::new(
            "inferscope.efficiency.energy_source",
            match eff.energy_source {
                is_core::EnergySource::Counter => "counter",
                is_core::EnergySource::IntegratedFallback => "integrated_fallback",
            },
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
    let run_end = run_start
        .checked_add(Duration::from_nanos(report.timing.total_latency_ns))
        .unwrap_or(run_start);
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

#[cfg(test)]
mod tests {
    //! These tests exercise the error-handling paths of
    //! [`export_to_otel`] without standing up a real OTLP
    //! collector. The happy-path is validated manually with a
    //! local Jaeger or OTel Collector during release qualification;
    //! see RUNBOOK.md.
    //!
    //! The test asserts only that an Err is returned, not which
    //! variant: opentelemetry-otlp 0.32 may catch malformed
    //! endpoints during builder construction (SetupFailed) or
    //! during the first flush (ExportFailed), and that detail is
    //! a transitive-dependency implementation choice we should not
    //! pin a test against.

    use super::*;
    use crate::{LatencyDistribution, TimingMetrics};
    use is_core::{RequestTiming, TokenArrival};

    /// Minimal Report with three token arrivals, no resource or GPU
    /// timeline. Just enough to exercise the span-construction path.
    fn sample_report() -> Report {
        Report {
            request_timing: RequestTiming::new(
                vec![
                    TokenArrival::new(0, 412_000_000),
                    TokenArrival::new(1, 458_000_000),
                    TokenArrival::new(2, 504_000_000),
                ],
                550_000_000,
            ),
            resource_timeline: None,
            gpu_timeline: None,
            timing: TimingMetrics {
                token_count: 3,
                ttft_ns: Some(412_000_000),
                total_latency_ns: 550_000_000,
                tokens_per_second: Some(21.74),
                inter_token_latency: Some(LatencyDistribution {
                    count: 2,
                    mean_ns: 46_000_000,
                    p50_ns: 46_000_000,
                    p95_ns: 46_000_000,
                    p99_ns: 46_000_000,
                    max_ns: 46_000_000,
                }),
            },
            resource: None,
            gpu: None,
            efficiency: None,
            kvcache_timeline: None,
            kvcache: None,
            phase_timeline: None,
            spec_timeline: None,
            phase_energy: None,
            reference_instant_unix_ns: None,
            trajectory: None,
            schema_version: Some(crate::metrics::REPORT_SCHEMA_VERSION),
        }
    }

    /// Build a single-threaded tokio runtime suitable for running
    /// `export_to_otel` synchronously in a unit test. The function
    /// builds a hyper client internally and requires an active tokio
    /// context.
    fn run_in_tokio<F, T>(f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");
        rt.block_on(async { f() })
    }

    /// An endpoint that is not a valid URL must produce an Err.
    /// We accept either SetupFailed (caught at builder time) or
    /// ExportFailed (caught at flush time).
    #[test]
    fn export_returns_err_for_invalid_endpoint_url() {
        let report = sample_report();
        let result = run_in_tokio(|| export_to_otel(&report, "not a url at all"));
        assert!(
            result.is_err(),
            "expected export to fail for malformed endpoint, got {result:?}"
        );
    }
}
