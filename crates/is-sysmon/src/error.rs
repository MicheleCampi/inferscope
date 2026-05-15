//! Error type for the sysmon layer.

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
