//! System monitor for inferscope.
//!
//! `is-sysmon` samples the inference engine process resource
//! footprint — RSS, CPU time, thread count — by reading `/proc`.
//! Samples carry timestamps measured from the same reference
//! instant the probe uses, so a sample can be correlated with a
//! token arrival by direct numeric comparison (see ADR-003).
//!
//! v0.1.0 covers the CPU-side story only. GPU sampling via NVML or
//! similar is explicitly out of scope and deferred to v0.2+.
//!
//! The crate is split into:
//!
//! - [`config`]: what to sample — PID and period.
//! - [`error`]: the failure modes of reading `/proc`.
//! - the sampling loop itself, which produces an
//!   [`is_core::ResourceTimeline`]. (Lands later in W3.)

pub mod config;
pub mod error;
pub mod parse;
pub mod sampler;

pub use config::SysmonConfig;
pub use error::SysmonError;
