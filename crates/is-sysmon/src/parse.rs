//! Parsers for the /proc files sysmon reads.
//!
//! Two files are parsed:
//!
//! - `/proc/<pid>/status` — a key-value text file. We extract
//!   `VmRSS` (resident set size, in kibibytes) and `Threads`.
//! - `/proc/<pid>/stat` — a single space-separated line with
//!   numeric fields. We extract field 14 (`utime`) and field 15
//!   (`stime`), both in scheduler jiffies.
//!
//! The parsers operate on `&str` content, not file paths. The
//! sampling loop reads the file into memory and hands the string
//! here. This keeps parsing pure and offline-testable: every edge
//! case is covered with synthetic input, with no `/proc` mock.

use crate::error::SysmonError;

/// Result of parsing /proc/<pid>/status for the fields sysmon needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusFields {
    /// Resident set size in bytes (converted from the kibibytes
    /// the kernel reports).
    pub rss_bytes: u64,
    /// Number of threads.
    pub thread_count: u32,
}

/// Result of parsing /proc/<pid>/stat for the fields sysmon needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatFields {
    /// User-mode CPU time in scheduler jiffies (field 14).
    pub cpu_user_jiffies: u64,
    /// Kernel-mode CPU time in scheduler jiffies (field 15).
    pub cpu_system_jiffies: u64,
}

/// Parses the body of `/proc/<pid>/status` for VmRSS and Threads.
///
/// The file is a sequence of `Key:\tValue` lines. We scan linearly
/// and return as soon as both target fields are found. Unknown
/// lines are ignored — kernels emit many fields and the set grows
/// over time.
pub fn parse_status(content: &str, path: &str) -> Result<StatusFields, SysmonError> {
    let mut rss_kib: Option<u64> = None;
    let mut threads: Option<u32> = None;

    for line in content.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            // Format: "VmRSS:\t   612672 kB"
            let trimmed = value.trim();
            let number_part = trimmed.split_whitespace().next().unwrap_or("");
            let parsed: u64 = number_part.parse().map_err(|_| SysmonError::InvalidValue {
                path: path.to_string(),
                field: "VmRSS",
                value: number_part.to_string(),
            })?;
            rss_kib = Some(parsed);
        } else if let Some(value) = line.strip_prefix("Threads:") {
            let number_part = value.trim();
            let parsed: u32 = number_part.parse().map_err(|_| SysmonError::InvalidValue {
                path: path.to_string(),
                field: "Threads",
                value: number_part.to_string(),
            })?;
            threads = Some(parsed);
        }

        if rss_kib.is_some() && threads.is_some() {
            break;
        }
    }

    let rss_kib = rss_kib.ok_or(SysmonError::MissingField {
        path: path.to_string(),
        field: "VmRSS",
    })?;
    let thread_count = threads.ok_or(SysmonError::MissingField {
        path: path.to_string(),
        field: "Threads",
    })?;

    Ok(StatusFields {
        // VmRSS is reported in kibibytes; convert to bytes for SI
        // comparability with other byte-counting fields in is-core.
        rss_bytes: rss_kib * 1024,
        thread_count,
    })
}

/// Parses the body of `/proc/<pid>/stat` for utime and stime.
///
/// The file is a single line of space-separated fields. The second
/// field is the command name wrapped in parentheses, and the
/// command itself can contain spaces and parentheses. We handle
/// this by skipping past the last `)` in the line and then
/// splitting the remainder on whitespace. After the closing
/// paren, the fields are: state (3), ppid (4), pgrp (5), session (6),
/// tty_nr (7), tpgid (8), flags (9), minflt (10), cminflt (11),
/// majflt (12), cmajflt (13), utime (14), stime (15)…
///
/// So in the slice after `)`, utime is index 11 (zero-based, after
/// state) and stime is index 12.
pub fn parse_stat(content: &str, path: &str) -> Result<StatFields, SysmonError> {
    let close_paren = content.rfind(')').ok_or(SysmonError::MissingField {
        path: path.to_string(),
        field: "comm_close_paren",
    })?;

    // Everything after `)` — note: also skips the space that
    // follows it.
    let after_comm = &content[close_paren + 1..];
    let fields: Vec<&str> = after_comm.split_whitespace().collect();

    // After `)` the fields start at "state". utime is field 14
    // overall, which is index 14 - 3 = 11 in this slice (we skipped
    // pid (1), comm (2), and the closing-paren delimiter). stime
    // is index 12.
    let utime_str = fields.get(11).ok_or(SysmonError::MissingField {
        path: path.to_string(),
        field: "utime",
    })?;
    let stime_str = fields.get(12).ok_or(SysmonError::MissingField {
        path: path.to_string(),
        field: "stime",
    })?;

    let cpu_user_jiffies: u64 = utime_str.parse().map_err(|_| SysmonError::InvalidValue {
        path: path.to_string(),
        field: "utime",
        value: utime_str.to_string(),
    })?;
    let cpu_system_jiffies: u64 = stime_str.parse().map_err(|_| SysmonError::InvalidValue {
        path: path.to_string(),
        field: "stime",
        value: stime_str.to_string(),
    })?;

    Ok(StatFields {
        cpu_user_jiffies,
        cpu_system_jiffies,
    })
}

/// Parses the body of `/proc/<pid>/task/<tid>/children` into the
/// list of direct child PIDs.
///
/// The file format is a single line of space-separated decimal
/// PIDs, possibly empty, possibly with trailing whitespace. The
/// kernel emits one PID per direct child of the given thread at
/// the moment of read; the snapshot is racy by nature (a child
/// can exit between this read and a subsequent `/proc/<child>`
/// access), and the caller is expected to tolerate per-PID
/// failures downstream — see ADR-006.
///
/// An empty file is a valid input and yields an empty `Vec`.
/// Any non-numeric token is reported as `InvalidValue`.
pub fn parse_children(content: &str, path: &str) -> Result<Vec<u32>, SysmonError> {
    content
        .split_whitespace()
        .map(|tok| {
            tok.parse::<u32>().map_err(|_| SysmonError::InvalidValue {
                field: "children_pid",
                path: path.to_string(),
                value: tok.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- status parsing -----

    #[test]
    fn parse_status_extracts_rss_and_threads() {
        let content = "\
Name:   test
Umask:  0022
State:  S (sleeping)
VmRSS:\t   612672 kB
VmData:    102400 kB
Threads:\t8
SigQ:   0/63881
";
        let got = parse_status(content, "/proc/1/status").unwrap();
        assert_eq!(got.rss_bytes, 612672 * 1024);
        assert_eq!(got.thread_count, 8);
    }

    #[test]
    fn parse_status_returns_early_after_both_fields_found() {
        // Even if the file is huge and has fields we don't care
        // about after VmRSS and Threads, parsing should succeed.
        // Concretely we don't reach lines after both targets, so a
        // malformed later line doesn't matter — but this is best
        // documented as a property, not pinned to a specific
        // implementation detail.
        let content = "VmRSS:\t1024 kB\nThreads:\t2\nGarbage:   not-a-number\n";
        let got = parse_status(content, "/proc/1/status").unwrap();
        assert_eq!(got.rss_bytes, 1024 * 1024);
        assert_eq!(got.thread_count, 2);
    }

    #[test]
    fn parse_status_errors_on_missing_vmrss() {
        let content = "Name: x\nThreads:\t1\n";
        let err = parse_status(content, "/proc/1/status").unwrap_err();
        assert!(
            matches!(err, SysmonError::MissingField { field: "VmRSS", .. }),
            "expected MissingField VmRSS, got {err:?}"
        );
    }

    #[test]
    fn parse_status_errors_on_missing_threads() {
        let content = "VmRSS:\t1024 kB\n";
        let err = parse_status(content, "/proc/1/status").unwrap_err();
        assert!(
            matches!(
                err,
                SysmonError::MissingField {
                    field: "Threads",
                    ..
                }
            ),
            "expected MissingField Threads, got {err:?}"
        );
    }

    #[test]
    fn parse_status_errors_on_unparseable_rss() {
        let content = "VmRSS:\txyz kB\nThreads:\t1\n";
        let err = parse_status(content, "/proc/1/status").unwrap_err();
        assert!(
            matches!(err, SysmonError::InvalidValue { field: "VmRSS", .. }),
            "expected InvalidValue VmRSS, got {err:?}"
        );
    }

    // ----- stat parsing -----

    #[test]
    fn parse_stat_extracts_utime_and_stime() {
        // Fields: pid (comm) state ppid pgrp session tty_nr tpgid
        // flags minflt cminflt majflt cmajflt utime stime ...
        //   1     2     3    4    5      6     7      8
        //   9       10      11      12     13     14    15
        let content = "1234 (test) S 1 1234 1234 0 -1 4194304 100 0 0 0 \
                       1234 56 0 0 20 0 1 0 1000 5000000 612 \
                       18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0\n";
        let got = parse_stat(content, "/proc/1/stat").unwrap();
        assert_eq!(got.cpu_user_jiffies, 1234);
        assert_eq!(got.cpu_system_jiffies, 56);
    }

    #[test]
    fn parse_stat_handles_command_with_spaces_and_parens() {
        // The comm field can contain spaces and parentheses. The
        // robust approach is to find the LAST `)`, not the first.
        let content = "1234 (rust llm (srv)) S 1 1234 1234 0 -1 4194304 100 0 0 0 \
                       1234 56 0 0 20 0 1 0 1000 5000000 612\n";
        let got = parse_stat(content, "/proc/1/stat").unwrap();
        assert_eq!(got.cpu_user_jiffies, 1234);
        assert_eq!(got.cpu_system_jiffies, 56);
    }

    #[test]
    fn parse_stat_errors_on_missing_close_paren() {
        let content = "1234 broken-line\n";
        let err = parse_stat(content, "/proc/1/stat").unwrap_err();
        assert!(
            matches!(
                err,
                SysmonError::MissingField {
                    field: "comm_close_paren",
                    ..
                }
            ),
            "expected MissingField comm_close_paren, got {err:?}"
        );
    }

    #[test]
    fn parse_stat_errors_when_utime_missing() {
        // A line ending early — only a few fields after the comm.
        let content = "1234 (test) S 1 1234 1234 0 -1\n";
        let err = parse_stat(content, "/proc/1/stat").unwrap_err();
        assert!(
            matches!(err, SysmonError::MissingField { field: "utime", .. }),
            "expected MissingField utime, got {err:?}"
        );
    }

    #[test]
    fn parse_stat_errors_on_unparseable_utime() {
        let content = "1234 (test) S 1 1234 1234 0 -1 4194304 100 0 0 0 \
                       xyz 56 0\n";
        let err = parse_stat(content, "/proc/1/stat").unwrap_err();
        assert!(
            matches!(err, SysmonError::InvalidValue { field: "utime", .. }),
            "expected InvalidValue utime, got {err:?}"
        );
    }

    #[test]
    fn parse_children_empty_returns_empty_vec() {
        let pids = parse_children("", "/proc/1/task/1/children").unwrap();
        assert!(pids.is_empty());
    }

    #[test]
    fn parse_children_single_pid() {
        let pids = parse_children("24865", "/proc/1/task/1/children").unwrap();
        assert_eq!(pids, vec![24865]);
    }

    #[test]
    fn parse_children_multiple_pids() {
        let pids = parse_children("24865 24866 24867", "/proc/1/task/1/children").unwrap();
        assert_eq!(pids, vec![24865, 24866, 24867]);
    }

    #[test]
    fn parse_children_tolerates_trailing_whitespace() {
        let pids = parse_children("24865 24866 \n", "/proc/1/task/1/children").unwrap();
        assert_eq!(pids, vec![24865, 24866]);
    }

    #[test]
    fn parse_children_errors_on_non_numeric_token() {
        let err = parse_children("24865 abc 24867", "/proc/1/task/1/children").unwrap_err();
        assert!(
            matches!(
                &err,
                SysmonError::InvalidValue { field: "children_pid", value, .. } if value == "abc"
            ),
            "expected InvalidValue children_pid with value=abc, got {err:?}"
        );
    }
}
