//! Core types for inferscope.
//!
//! This crate defines the shared vocabulary used across the inferscope
//! workspace: the raw timing data captured when probing an inference
//! engine, the report structures that aggregate it, and the error type
//! common to the core layer.
//!
//! `is-core` deliberately contains no I/O and no async code. It is a pure
//! data-definition crate so that every other crate in the workspace can
//! depend on it without pulling in a runtime.

pub mod error;
pub mod gpu;
pub mod kvcache;
pub mod resource;
pub mod timing;

pub use error::CoreError;
pub use gpu::{DeviceEnergy, EnergySource, GpuSample, GpuTimeline};
pub use kvcache::{KvCacheSample, KvCacheTimeline};
pub use resource::{ResourceSample, ResourceTimeline};
pub use timing::{RequestTiming, TokenArrival};
