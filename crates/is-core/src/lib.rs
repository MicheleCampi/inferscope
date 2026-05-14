//! Core types for inferscope.
//!
//! This crate defines the shared vocabulary used across the inferscope
//! workspace: the metric types produced by probing an inference engine,
//! the report structures that aggregate them, and the error type common
//! to the core layer.
//!
//! `is-core` deliberately contains no I/O and no async code. It is a pure
//! data-definition crate so that every other crate in the workspace can
//! depend on it without pulling in a runtime.

pub mod error;

pub use error::CoreError;
