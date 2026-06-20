# Architecture Decision Records

This directory records the significant architectural decisions made
on inferscope: the context that motivated each one, the decision
itself, the alternatives considered, and the consequences.

ADRs are **immutable once accepted**. If a decision is later
reversed, a new ADR is added that supersedes the old one; the old
ADR's status is updated to point at its successor, but its text is
left intact as a historical record.

## Index

| ID | Title | Status |
|----|-------|--------|
| [ADR-001](001-profiling-scope.md) | Profiling Scope for v0.1.0 | Accepted |
| [ADR-002](002-token-timing-representation.md) | Token Timing Representation | Accepted |
| [ADR-003](003-sysmon-scope-and-correlation.md) | sysmon Scope and Temporal Correlation | Accepted |
| [ADR-004](004-report-metrics-and-format.md) | Report Metrics and Output Format | Accepted |
| [ADR-005](005-gpu-resource-sampling.md) | GPU Resource Sampling | Accepted |
| [ADR-006](006-process-tree-aggregation.md) | Process Tree Aggregation for Sysmon | Accepted |
| [ADR-007](007-per-device-gpu-metrics.md) | Per-Device GPU Metrics in the Report Schema | Accepted |
| [ADR-008](008-opentelemetry-export.md) | OpenTelemetry Export of Inferscope Reports | Accepted |
| [ADR-009](009-sample-only-mode.md) | Sample-Only Mode | Accepted |
| [ADR-010](010-energy-and-efficiency-metrics.md) | Energy Consumption and Efficiency Metrics | Accepted |

## Format

ADRs follow the structure proposed by Michael Nygard in
[Documenting Architecture Decisions](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions):

- **Status** — Proposed / Accepted / Deprecated / Superseded
- **Context** — the situation that requires a decision
- **Decision** — what was decided
- **Consequences** — positive and negative outcomes
- **Alternatives Considered** — what was not chosen, and why

New ADRs are numbered sequentially (`002-...`, `003-...`).
