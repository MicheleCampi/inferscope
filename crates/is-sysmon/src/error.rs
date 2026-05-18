//! Error types for the sysmon layer.

/// Errors that can occur while sampling an engine process via /proc.
///
/// The variants are organised by failure stage so a caller can tell
/// "the process is gone" from "the kernel data format changed under
/// us" from "an I/O glitch happened reading the file".
#[derive(Debug, thiserror::Error)]
pub enum SysmonError {
    /// The /proc file for the process could not be read. The most
    /// common cause is that the process exited between sampling
    /// ticks; the path is included so the caller can confirm what
    /// was attempted.
    #[error("failed to read {path}: {source}")]
    Io {
        /// The /proc path that was being read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A required field was missing from a /proc file. This means
    /// the data format diverged from what sysmon expects — for
    /// example, an unusual kernel build, or a future change to the
    /// /proc layout.
    #[error("missing field {field} in {path}")]
    MissingField {
        /// The /proc path that was being parsed.
        path: String,
        /// The name of the field that was not found.
        field: &'static str,
    },

    /// A field was present but its value did not parse as the
    /// expected numeric type.
    #[error("invalid value for field {field} in {path}: {value}")]
    InvalidValue {
        /// The /proc path that was being parsed.
        path: String,
        /// The name of the field whose value did not parse.
        field: &'static str,
        /// The raw text of the value that failed to parse.
        value: String,
    },
}

/// Errors that can occur while initialising or running the GPU
/// sampler.
///
/// Per ADR-005, the variants distinguish between "no GPU sampling
/// is possible on this host" (recoverable: orchestrator skips GPU
/// sampling) and "we have a driver but a specific query failed"
/// (also recoverable: we drop the affected sample, not the whole
/// run).
///
/// The type is only compiled when the `gpu-nvidia` Cargo feature
/// is enabled; with the feature off, no GPU code is in the binary.
#[cfg(feature = "gpu-nvidia")]
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// NVML could not be loaded or initialised. The typical cause
    /// is that the NVIDIA driver is not installed on this host
    /// (the dlopen of `libnvidia-ml.so.1` fails). The `details`
    /// field carries the diagnostic from the underlying
    /// `nvml-wrapper` error.
    ///
    /// Per ADR-005, this is not a fatal error: the orchestrator
    /// proceeds with CPU-only sampling and notes the absence in
    /// the report.
    #[error("NVML unavailable: {details}")]
    NvmlUnavailable {
        /// Diagnostic from the underlying NVML loader.
        details: String,
    },

    /// A NVML query failed during sampler initialisation —
    /// specifically while enumerating devices. This is different
    /// from `NvmlUnavailable`: NVML loaded, but a follow-up call
    /// did not return the expected information. Rare on
    /// well-configured hosts.
    #[error("NVML query failed during {stage}: {details}")]
    DeviceQueryFailed {
        /// Which initialisation stage failed: typically
        /// `"device_count"` or `"device_by_index"`.
        stage: &'static str,
        /// Diagnostic from the underlying NVML call.
        details: String,
    },
}
