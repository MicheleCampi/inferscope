# ADR-007: Per-Device GPU Metrics in the Report Schema

- **Status**: Proposed
- **Date**: 2026-05-24
- **Deciders**: Michele Campi

## Context

ADR-005 established the GPU sampling contract: sample NVML once per
tick per visible device, store the raw readings in
`gpu_timeline.samples[]` with a `device_index` field, and emit
derived aggregates in `gpu.*`. The aggregates were chosen to match
the shape of the existing CPU `resource.*` aggregates: max VRAM,
mean utilization, mean power, peak temperature, all collapsed to a
single number across the entire sample stream.

The single-number contract was correct for the v0.2.0 RTX L4
validation, which exercised one GPU only, and remained adequate for
the v0.2.1 H100 single-GPU runs. It became visibly inadequate during
the 23 May 2026 4×A40 multi-GPU validation. With `tensor-split` on
two of four GPUs (the TP=2 case), the workload touches devices 0 and
1 while devices 2 and 3 sit idle. The aggregate `gpu.utilization_max_percent`
reports the momentary peak of one busy device; `gpu.memory_used_max_bytes`
reports the per-sample max across all devices (the largest single GPU
sample); `gpu.power_mean_milliwatts` averages two busy GPUs with two
idle ones. A consumer reading the JSON cannot tell that two of the
four GPUs are doing all the work. The per-device data is in
`gpu_timeline.samples[]` — every sample has its `device_index` — but
the consumer must regroup the samples by hand to reconstruct what
the human-readable summary report already shows for free.

The pattern is identical to the one ADR-006 addressed for `/proc`
aggregation: the raw data captures the right thing, but the derived
report layer collapses the dimension the consumer needs. ADR-006
fixed sysmon by adding `--include-descendants` to sum across the
process tree; this ADR fixes the GPU dimension by exposing per-device
metrics in the derived aggregate section.

The motivating use case is the `benchmarks/multi-device-validation.md`
file shipped 24 May 2026, which has to pull per-device numbers from
the summary report rather than the JSON, and document the workaround
inline. That file will be the first consumer of the new schema.

## Decision

Add a `per_device: Vec<GpuDeviceMetrics>` field to the existing
`GpuMetrics` struct in `is-report::metrics`. Populate it during
`derive_gpu()` by grouping `gpu_timeline.samples[]` by `device_index`
and computing the same set of aggregates per group that the
top-level `gpu.*` currently computes across all samples.

The existing top-level `gpu.*` aggregate fields remain unchanged.
They continue to represent cluster-wide max/mean/min across all
samples regardless of device, for backward compatibility with any
consumer that depends on the v0.2 schema. The new `per_device`
field is additive.

Schema shape (JSON):

```json
{
  "gpu": {
    "sample_count": 432,
    "device_count": 4,
    "memory_used_max_bytes": 3076..,     // cluster-wide, unchanged
    "utilization_mean_percent": 18,      // cluster-wide, unchanged
    "power_mean_milliwatts": 89400,      // cluster-wide, unchanged
    "...": "...",
    "per_device": [
      {
        "device_index": 0,
        "sample_count": 108,
        "memory_used_max_bytes": 2748..,
        "utilization_mean_percent": 33,
        "power_mean_milliwatts": 148000,
        "temperature_max_celsius": 52
      },
      {
        "device_index": 1,
        "sample_count": 108,
        "memory_used_max_bytes": 3081..,
        "utilization_mean_percent": 38,
        "power_mean_milliwatts": 152500,
        "temperature_max_celsius": 53
      },
      {
        "device_index": 2,
        "sample_count": 108,
        "memory_used_max_bytes": 0,
        "utilization_mean_percent": 0,
        "power_mean_milliwatts": 34100,
        "temperature_max_celsius": 33
      },
      {
        "device_index": 3,
        "sample_count": 108,
        "memory_used_max_bytes": 0,
        "utilization_mean_percent": 0,
        "power_mean_milliwatts": 32300,
        "temperature_max_celsius": 34
      }
    ]
  }
}
```

The same set of aggregates appears in both top-level and per-device
form: max/min/mean for VRAM, max/min/mean for utilization, max/mean
for power, max for temperature, plus a `sample_count` for each
device. There is no `device_count` inside each per_device entry —
that is a cluster-level fact.

The Rust type:

```rust
pub struct GpuMetrics {
    // ... existing cluster-wide fields unchanged ...
    pub per_device: Vec<GpuDeviceMetrics>,
}

pub struct GpuDeviceMetrics {
    pub device_index: u32,
    pub sample_count: usize,
    pub memory_used_min_bytes: u64,
    pub memory_used_max_bytes: u64,
    pub memory_used_mean_bytes: u64,
    pub utilization_min_percent: u32,
    pub utilization_max_percent: u32,
    pub utilization_mean_percent: u32,
    pub power_max_milliwatts: u32,
    pub power_mean_milliwatts: u32,
    pub temperature_max_celsius: u32,
}
```

The text report adds a "Per-device GPU" block after the existing
GPU block, with one indented section per device, only when
`device_count > 1`. Single-GPU runs produce identical output to
v0.2.x (no per-device block) to keep typical output noise-free.

## Consequences

**Positive:**

- Multi-GPU runs become self-describing in the JSON. The consumer
  no longer has to regroup `gpu_timeline.samples[]` to see that
  GPU 2 and GPU 3 were idle on a TP=2 run. The same JSON section
  that today shows aggregate "cluster max 3.08 GB" will also show
  "GPU 0: 2.56 GB, GPU 1: 2.87 GB, GPU 2: 0 GB, GPU 3: 0 GB".
- `benchmarks/multi-device-validation.md` and any future multi-GPU
  benchmark can cite JSON fields directly instead of human-readable
  summary lines. The "until ADR-007 lands" caveat in that file gets
  removed.
- Single-GPU runs are unchanged. Existing consumers parsing v0.2
  JSON keep working without modification.

**Negative:**

- JSON output size grows. For 4×A40, the `per_device` array adds
  roughly 600 bytes (4 entries × ~150 bytes each). On a typical
  run JSON of 40-60 KB this is a 1-2 % increase, not material.
  Single-GPU runs add ~150 bytes (one entry), also immaterial.
- The text report grows visually for multi-GPU runs. This is the
  point — that's the information the consumer wants — but operators
  with tight terminal output expectations may notice.
- One additional derived aggregation pass over the sample timeline
  at end of run. Performance cost: negligible (we already iterate
  the timeline once for cluster aggregation; a second pass grouped
  by `device_index` adds microseconds to a process whose runtime
  is dominated by HTTP I/O).

**Neutral:**

- The schema change is purely additive at the JSON level. No
  existing fields are removed, renamed, or have their semantics
  changed. Consumers that only read top-level `gpu.*` continue
  to work. This is not a breaking change; it does not require
  a major version bump under SemVer. v0.3.0 is justified by the
  feature addition, not by a compatibility break.

## Alternatives Considered

**1. Replace top-level aggregates with `per_device` only.** Make
the JSON exclusively per-device, with the consumer summing or
averaging if they want cluster-wide numbers. Rejected: this would
be a breaking change for v0.2 consumers (including `inferscope`'s
own report text), and the cluster-wide view is genuinely useful
for single-GPU runs and balanced multi-GPU runs. Keeping both
costs ~150 bytes per device and zero ambiguity.

**2. Add a `--per-device` CLI flag, gate the new output behind it.**
Make per-device opt-in to preserve the v0.2 default exactly.
Rejected: this leaves the multi-GPU JSON misleading by default
for any consumer that doesn't know to pass the flag, which is
the same usability problem we're trying to fix. The pattern from
ADR-006 (`--include-descendants` opt-in) made sense there because
sysmon descendant aggregation has security implications (sums
across processes the user may not own) — GPU per-device aggregation
has no such concern.

**3. Move `per_device` to a new top-level field, parallel to `gpu`.**
For example, `"per_device_gpu": [...]` at JSON root. Rejected:
this creates two GPU sections with the same `sample_count`
inconsistencies under partial sampling and makes the data layout
non-hierarchical for no clear reader benefit. The cluster aggregate
and the per-device aggregate are the same kind of derived view of
the same raw stream; they belong in the same JSON object.

**4. Compute per-device aggregates lazily on demand via a CLI
subcommand (e.g. `inferscope analyze --device 0 run.json`).**
Rejected: this duplicates logic that already exists (the human
report computes per-device), pushes work onto every consumer that
wants the data, and conflicts with the existing JSON-as-canonical
output contract. The raw timeline samples are already in the JSON
for anyone who wants to do bespoke analysis; the derived aggregate
should match the human report.
