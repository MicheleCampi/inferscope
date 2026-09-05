//! Prometheus metric source for inferscope.
//!
//! `is-metrics` scrapes an inference engine's Prometheus `/metrics`
//! endpoint over HTTP and reads three independent signals off the body:
//! the KV-cache hit-rate counters (ADR-011), the per-phase token and
//! timing series (ADR-012), and the speculative-decoding counters
//! (ADR-016). Each has its own scrape loop over the same run window, on
//! the same clock and cancel signal but not the same GET. It is
//! inferscope's first
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
//!
//! A private `schema` module holds the metric vocabulary each engine
//! speaks (ADR-014). It stays internal: series names are how this crate
//! reads a body, not something a caller declares or a report carries.

pub mod config;
pub mod error;
pub mod parse;
pub mod scrape;

mod schema;

pub use config::{Engine, MetricsConfig};
pub use error::MetricsError;
// All three parsers are re-exported, not only the KV one. The partial
// re-export was a residue of when parse_kvcache was the module's only
// function, not a statement that the other two are less public: the three
// read three independent signals off the same body and sit on one level.
pub use parse::{parse_kvcache, parse_phase, parse_spec, SpecReading};
pub use scrape::{
    scrape_during, scrape_once, scrape_phase_during, scrape_phase_once, scrape_spec_during,
    scrape_spec_once,
};
