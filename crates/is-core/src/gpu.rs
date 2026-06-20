//! Raw GPU resource samples for a monitored engine process.
//!
//! These types hold the GPU-side resource footprint of an
//! inference engine — VRAM used, SM utilisation, temperature,
//! power draw — while a probe run is in progress. Sampled in
//! parallel with the CPU-side [`crate::ResourceSample`] stream
//! and correlated by the same `elapsed_ns` scheme (see ADR-005).
//!
//! Per ADR-005, values are stored in the integer units the
//! underlying APIs (NVML, ROCm SMI) expose them in — bytes for
//! memory, percent as a 0–100 integer, celsius, milliwatts —
//! preserving precision at the data layer and deferring
//! conversion to the reporting layer.

use serde::{Deserialize, Serialize};

/// Where an energy figure came from.
///
/// Per ADR-010, the NVML hardware counter
/// (`nvmlDeviceGetTotalEnergyConsumption`) is the primary source and is
/// preferred when available. When the counter is unavailable (pre-Volta
/// hardware, or `NVML_ERROR_NOT_SUPPORTED`), energy is estimated by
/// trapezoidal integration of the power samples, which is explicitly
/// second-best. The source is recorded so a consumer never confuses an
/// integrated estimate with a counter reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnergySource {
    /// Delta of the NVML hardware energy counter over the window.
    Counter,
    /// Trapezoidal integral of the power samples (fallback).
    IntegratedFallback,
}

/// Energy consumed by one GPU device over the measurement window.
///
/// This is a window-level quantity, not a per-tick sample: it is the
/// delta of the device's cumulative energy between the start and end of
/// the run (see ADR-010). Stored in integer millijoules, consistent with
/// the milliwatt power unit of [`GpuSample`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEnergy {
    /// GPU index on the host, 0-based; matches [`GpuSample::device_index`].
    pub device_index: u32,

    /// Energy consumed over the window, in millijoules.
    pub energy_millijoules: u64,

    /// How this figure was obtained (counter or integrated fallback).
    pub source: EnergySource,
}

/// A single sample of one GPU device's resource state at one
/// moment in time.
///
/// `elapsed_ns` is nanoseconds since the same reference instant
/// the CPU sampler uses, so a GPU sample can be correlated with
/// a token arrival or a CPU sample by direct numeric comparison
/// (see ADR-003 and ADR-005).
///
/// On a multi-GPU host the GPU sampler emits one sample per
/// device per tick. Consumers filter or aggregate by
/// `device_index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuSample {
    /// Nanoseconds from the reference instant to when this sample
    /// was taken.
    pub elapsed_ns: u64,

    /// GPU index on the host, 0-based. A single-GPU machine
    /// always reports 0; a multi-GPU machine emits one sample
    /// per index per tick.
    pub device_index: u32,

    /// VRAM currently allocated on the device, in bytes
    /// (`nvmlDeviceGetMemoryInfo.used` on NVIDIA;
    /// `rsmi_dev_memory_usage_get` on AMD).
    pub memory_used_bytes: u64,

    /// VRAM capacity of the device, in bytes. Constant across
    /// ticks for the same device but emitted on every sample so
    /// a consumer reading a single sample knows the device's
    /// total memory without reaching for separate metadata.
    pub memory_total_bytes: u64,

    /// SM utilisation (NVIDIA) or GPU usage percent (AMD), 0–100.
    /// Reported as an integer because the underlying APIs expose
    /// it as such.
    pub utilization_percent: u8,

    /// Current chip temperature in degrees Celsius.
    pub temperature_celsius: u32,

    /// Current power draw in milliwatts. Stored as integer
    /// milliwatts (not float watts) to preserve the precision of
    /// the underlying API.
    pub power_draw_milliwatts: u32,
}

/// A complete timeline of GPU samples for one probe run.
///
/// Samples are kept in the order they were taken. On a multi-GPU
/// host the timeline is interleaved by device index within each
/// tick: device 0's sample, then device 1's, then 0 again on the
/// next tick. Consumers that want a per-device view filter by
/// `device_index`.
///
/// `sample_period_ns` records the nominal sampling period the
/// GPU sampler used. Actual gaps vary; the field is
/// informational.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuTimeline {
    /// The samples, in order of insertion.
    pub samples: Vec<GpuSample>,

    /// The nominal sampling period the GPU sampler was configured
    /// with, in nanoseconds.
    pub sample_period_ns: u64,

    /// Per-device energy over the measurement window (see ADR-010).
    ///
    /// `None` when no energy was measured — a run with the GPU sampler
    /// disabled, or a build without the `gpu-nvidia` feature. Older
    /// reports written before ADR-010 deserialize with this absent, so
    /// the field is backward-compatible in both directions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy: Option<Vec<DeviceEnergy>>,
}

impl GpuTimeline {
    /// Creates an empty timeline with the given nominal period.
    pub fn new(sample_period_ns: u64) -> Self {
        Self {
            samples: Vec::new(),
            sample_period_ns,
            energy: None,
        }
    }

    /// Returns the total number of samples across all devices.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Returns `true` if no samples were taken.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Appends a sample. Caller is responsible for sample order:
    /// in normal use samples are pushed in `elapsed_ns` order,
    /// with multi-device ticks contributing N consecutive samples
    /// sharing the same `elapsed_ns`.
    pub fn push(&mut self, sample: GpuSample) {
        self.samples.push(sample);
    }

    /// Returns the set of distinct device indices that appear in
    /// the timeline, in ascending order.
    ///
    /// A single-GPU run returns `[0]`. A 4-GPU run returns
    /// `[0, 1, 2, 3]`. An empty timeline returns `[]`.
    pub fn device_indices(&self) -> Vec<u32> {
        let mut indices: Vec<u32> = self.samples.iter().map(|s| s.device_index).collect();
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(elapsed_ns: u64, device_index: u32, mem_used: u64) -> GpuSample {
        GpuSample {
            elapsed_ns,
            device_index,
            memory_used_bytes: mem_used,
            memory_total_bytes: 80 * 1024 * 1024 * 1024, // 80 GB A100
            utilization_percent: 50,
            temperature_celsius: 45,
            power_draw_milliwatts: 250_000,
        }
    }

    #[test]
    fn timeline_starts_empty() {
        let t = GpuTimeline::new(50_000_000);
        assert_eq!(t.len(), 0);
        assert!(t.is_empty());
        assert_eq!(t.sample_period_ns, 50_000_000);
    }

    #[test]
    fn push_appends_samples() {
        let mut t = GpuTimeline::new(50_000_000);
        t.push(sample(50_000_000, 0, 1_000_000_000));
        t.push(sample(100_000_000, 0, 2_000_000_000));
        t.push(sample(150_000_000, 0, 3_000_000_000));
        assert_eq!(t.len(), 3);
        assert_eq!(t.samples[0].memory_used_bytes, 1_000_000_000);
        assert_eq!(t.samples[2].memory_used_bytes, 3_000_000_000);
    }

    #[test]
    fn device_indices_single_gpu() {
        let mut t = GpuTimeline::new(50_000_000);
        t.push(sample(50_000_000, 0, 100));
        t.push(sample(100_000_000, 0, 200));
        assert_eq!(t.device_indices(), vec![0]);
    }

    #[test]
    fn device_indices_multi_gpu_sorted_and_deduped() {
        let mut t = GpuTimeline::new(50_000_000);
        // One tick of a 4-GPU machine: four samples at the same
        // elapsed_ns, one per device, possibly out of index order.
        t.push(sample(50_000_000, 2, 100));
        t.push(sample(50_000_000, 0, 100));
        t.push(sample(50_000_000, 3, 100));
        t.push(sample(50_000_000, 1, 100));
        // Second tick of the same 4 GPUs.
        t.push(sample(100_000_000, 0, 200));
        t.push(sample(100_000_000, 1, 200));
        t.push(sample(100_000_000, 2, 200));
        t.push(sample(100_000_000, 3, 200));
        assert_eq!(t.device_indices(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn device_indices_empty_timeline() {
        let t = GpuTimeline::new(50_000_000);
        assert_eq!(t.device_indices(), Vec::<u32>::new());
    }

    #[test]
    fn gpu_sample_survives_json_round_trip() {
        let original = GpuSample {
            elapsed_ns: 412_000_000,
            device_index: 0,
            memory_used_bytes: 32 * 1024 * 1024 * 1024,
            memory_total_bytes: 80 * 1024 * 1024 * 1024,
            utilization_percent: 87,
            temperature_celsius: 52,
            power_draw_milliwatts: 312_500,
        };

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: GpuSample = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn gpu_timeline_survives_json_round_trip() {
        let mut original = GpuTimeline::new(50_000_000);
        original.push(sample(50_000_000, 0, 1_000_000_000));
        original.push(sample(50_000_000, 1, 2_000_000_000));
        original.push(sample(100_000_000, 0, 1_500_000_000));
        original.push(sample(100_000_000, 1, 2_500_000_000));

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: GpuTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn device_energy_source_serialises_snake_case() {
        let counter = serde_json::to_string(&EnergySource::Counter).expect("serialize");
        let fallback =
            serde_json::to_string(&EnergySource::IntegratedFallback).expect("serialize");
        assert_eq!(counter, "\"counter\"");
        assert_eq!(fallback, "\"integrated_fallback\"");
    }

    #[test]
    fn timeline_with_energy_survives_json_round_trip() {
        let mut original = GpuTimeline::new(500_000_000);
        original.push(sample(500_000_000, 0, 1_000_000_000));
        original.push(sample(1_000_000_000, 0, 1_000_000_000));
        original.energy = Some(vec![
            DeviceEnergy {
                device_index: 0,
                energy_millijoules: 51_500,
                source: EnergySource::Counter,
            },
            DeviceEnergy {
                device_index: 1,
                energy_millijoules: 56_000,
                source: EnergySource::IntegratedFallback,
            },
        ]);

        let json = serde_json::to_string(&original).expect("serialize");
        let restored: GpuTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn pre_adr010_timeline_without_energy_field_deserialises_to_none() {
        // A GpuTimeline JSON written before ADR-010 has no `energy` key.
        // It must deserialise with energy = None, not error.
        let legacy = r#"{"samples":[],"sample_period_ns":500000000}"#;
        let restored: GpuTimeline = serde_json::from_str(legacy).expect("deserialize legacy");
        assert_eq!(restored.energy, None);
        assert_eq!(restored.sample_period_ns, 500_000_000);
    }

    #[test]
    fn timeline_without_energy_omits_field_in_json() {
        // skip_serializing_if: a None energy must not appear in output.
        let t = GpuTimeline::new(500_000_000);
        let json = serde_json::to_string(&t).expect("serialize");
        assert!(!json.contains("energy"), "energy must be omitted when None: {json}");
    }
}
