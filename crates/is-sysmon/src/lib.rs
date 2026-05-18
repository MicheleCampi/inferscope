//! System monitor for inferscope.
//!
//! `is-sysmon` samples the inference engine process resource
//! footprint — RSS, CPU time, thread count — by reading `/proc`.
//! Samples carry timestamps measured from the same reference
//! instant the probe uses, so a sample can be correlated with a
//! token arrival by direct numeric comparison (see ADR-003).
//!
//! GPU sampling lands in v0.2 (see ADR-005) and is gated behind
//! the `gpu-nvidia` Cargo feature. With the feature off the crate
//! behaves exactly as v0.1.0; with the feature on,
//! [`gpu_nvidia::GpuSampler`] becomes available alongside the
//! `/proc` sampler. AMD support is planned behind a parallel
//! `gpu-amd` feature.
//!
//! The crate is split into:
//!
//! - [`config`]: what to sample — PID and period.
//! - [`error`]: the failure modes of reading `/proc` (always),
//!   plus GPU error variants when GPU features are enabled.
//! - [`parse`]: pure parsing of `/proc` text content.
//! - [`sampler`]: the CPU-side sampling loop.
//! - [`gpu_nvidia`] (feature `gpu-nvidia`): NVML-based GPU
//!   sampling, parallel in shape to [`sampler`].

pub mod config;
pub mod error;
pub mod parse;
pub mod sampler;

#[cfg(feature = "gpu-nvidia")]
pub mod gpu_nvidia;

pub use config::SysmonConfig;
pub use error::SysmonError;

#[cfg(feature = "gpu-nvidia")]
pub use error::GpuError;

#[cfg(feature = "gpu-nvidia")]
pub use gpu_nvidia::{sample_gpu_during, GpuSampler};
