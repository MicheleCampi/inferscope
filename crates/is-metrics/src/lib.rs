//! Prometheus metric source for inferscope.
//!
//! `is-metrics` scrapes an inference engine's Prometheus `/metrics`
//! endpoint over HTTP and captures the KV-cache hit-rate counters a
//! vLLM-schema engine exposes — `vllm:prefix_cache_hits` and
//! `vllm:prefix_cache_queries` (see ADR-011). It is inferscope's first
//! *application-internal* metric source: unlike `is-sysmon`, which reads
//! the engine process from the host via `/proc` and NVML, this crate
//! reads counters from inside the engine across a network boundary.
//!
//! It mirrors the shape `is-sysmon` established: a scrape loop that
//! samples at a configured cadence on the same reference `Instant` the
//! probe uses (ADR-003), accumulating raw samples into an
//! [`is_core::KvCacheTimeline`]. The loop is best-effort — a failed
//! scrape is swallowed and the timeline continues — so a transient
//! endpoint hiccup yields a partial timeline rather than aborting the
//! run.
//!
//! The crate is split into:
//!
//! - [`config`]: what to scrape — endpoint, model label, period.
//! - [`error`]: the failure modes of an HTTP scrape (transport,
//!   non-success status, parse).

pub mod config;
pub mod error;
pub mod parse;
pub mod scrape;

pub use config::MetricsConfig;
pub use error::MetricsError;
pub use parse::parse_kvcache;
pub use scrape::{scrape_during, scrape_once};
