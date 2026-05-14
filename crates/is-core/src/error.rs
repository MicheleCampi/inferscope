//! Error type for the core layer.

/// Errors originating from the core data layer.
///
/// At v0.1.0 the core layer does very little that can fail, but having a
/// dedicated error type from the start keeps the workspace consistent
/// with the per-crate error strategy and avoids a breaking change later.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A metric value was outside the range considered valid.
    #[error("invalid metric value for {field}: {reason}")]
    InvalidMetric {
        /// The field that held the invalid value.
        field: String,
        /// Why the value was rejected.
        reason: String,
    },
}
