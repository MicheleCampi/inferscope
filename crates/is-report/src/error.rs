//! Error type for the report layer.

/// Errors that can occur while deriving metrics or rendering a
/// report.
///
/// The report layer does no I/O, so most variants describe
/// preconditions that the input data must satisfy before metrics
/// can be derived. Each variant carries enough context to identify
/// what was missing.
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    /// CPU utilisation cannot be computed because the system's
    /// `_SC_CLK_TCK` could not be read or was zero. Without it,
    /// jiffies cannot be converted to seconds.
    #[error("could not determine system clock tick rate (_SC_CLK_TCK)")]
    UnknownClockTick,
}
