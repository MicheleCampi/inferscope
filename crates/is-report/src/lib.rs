//! Report generation for inferscope.
//!
//! `is-report` is the pure presentation layer. It takes the raw
//! signals captured by [`is_core::RequestTiming`] and
//! [`is_core::ResourceTimeline`], derives metrics from them, and
//! renders the result as either plain text for terminal viewing
//! or JSON for programmatic consumption.
//!
//! This crate does no I/O. Every operation is a pure function of
//! its inputs.
//!
//! The crate is split into:
//!
//! - [`error`]: the failure modes that can occur during derivation.
//! - [`metrics`]: the derived metric types — timing, resource, and
//!   the combined [`metrics::Report`].
//! - `derive`: the functions that compute metrics from the raw
//!   signals. (Lands later in W4.)
//! - `render`: text and JSON rendering. (Lands later in W4.)

pub mod error;
pub mod metrics;

pub use error::ReportError;
pub use metrics::{LatencyDistribution, Report, ResourceMetrics, TimingMetrics};
