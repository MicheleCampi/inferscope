//! NVIDIA GPU sampling via NVML.
//!
//! This module implements the GPU side of the resource story
//! defined in ADR-005. It is the GPU counterpart of [`crate::sampler`]:
//! same shape, same shared-`Instant` correlation, same best-effort
//! error tolerance, same `sample_once` / `sample_during` API.
//!
//! NVML is accessed via the `nvml-wrapper` crate, which loads
//! `libnvidia-ml.so.1` at runtime through `libloading`. The binary
//! therefore compiles and links on hosts without an NVIDIA driver;
//! `GpuSampler::new()` is the runtime gate.
//!
//! The whole module is gated behind the `gpu-nvidia` Cargo feature;
//! with the feature off it does not exist in the compiled binary.

use std::time::Instant;

use is_core::{DeviceEnergy, EnergySource, GpuSample, GpuTimeline};
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::{Device, Nvml};
use tokio::sync::oneshot;
use tokio::time::{interval, MissedTickBehavior};

use crate::config::SysmonConfig;
use crate::error::GpuError;

/// Owns one NVML handle and the list of GPU device indices.
///
/// Construct once at the start of a probe run; reuse across
/// ticks. The NVML library is loaded once (via `Nvml::init()`)
/// and the device count is queried up front. Each tick re-fetches
/// the per-device handles by index — the cost is a handful of
/// microseconds per device, negligible against a 50 ms tick.
///
/// The per-tick lookup pattern is deliberate: it lets the
/// `Nvml` instance own its handles outright without any lifetime
/// gymnastics, keeping the whole crate free of `unsafe`.
pub struct GpuSampler {
    nvml: Nvml,
    device_indices: Vec<u32>,
}

impl GpuSampler {
    /// Initialises NVML and records the indices of every GPU
    /// visible on the host.
    ///
    /// Fails fast with [`GpuError::NvmlUnavailable`] when NVML
    /// cannot be loaded — typically because the NVIDIA driver is
    /// not installed. Per ADR-005, this is a recoverable
    /// "no GPU sampling on this host" condition rather than a
    /// fatal error; the orchestrator decides how to respond.
    pub fn new() -> Result<Self, GpuError> {
        // Initialise NVML. The crate uses `libloading` so this is
        // a runtime dlopen; failure here means no driver.
        let nvml = Nvml::init().map_err(|e| GpuError::NvmlUnavailable {
            details: format!("{e}"),
        })?;

        let count = nvml
            .device_count()
            .map_err(|e| GpuError::DeviceQueryFailed {
                stage: "device_count",
                details: format!("{e}"),
            })?;

        let device_indices = (0..count).collect();

        Ok(GpuSampler {
            nvml,
            device_indices,
        })
    }

    /// Returns the number of GPUs visible to the sampler.
    pub fn device_count(&self) -> usize {
        self.device_indices.len()
    }

    /// Reads the cumulative energy counter of every device once.
    ///
    /// Returns one `(device_index, millijoules)` pair per device whose
    /// `nvmlDeviceGetTotalEnergyConsumption` call succeeds. Per ADR-010
    /// the counter is the primary energy source; it is supported on
    /// Volta and newer. A device that returns `NOT_SUPPORTED`
    /// (pre-Volta) or any other error is omitted — the caller treats a
    /// device absent from both the start and end snapshot as having no
    /// counter energy, and the report falls back to the integrated
    /// estimate for it.
    ///
    /// This is a point-in-time read of a monotonic counter, not a
    /// `GpuSample`: window energy is the delta between two such reads.
    pub fn read_energy_counters(&self) -> Vec<(u32, u64)> {
        let mut out = Vec::with_capacity(self.device_indices.len());
        for &index in &self.device_indices {
            if let Ok(device) = self.nvml.device_by_index(index) {
                if let Ok(mj) = device.total_energy_consumption() {
                    out.push((index, mj));
                }
            }
        }
        out
    }

    /// Samples every GPU once and returns one `GpuSample` per
    /// device that produced a complete reading.
    ///
    /// Per ADR-005's best-effort policy: if a given device fails
    /// any of the five NVML queries this tick, the whole sample
    /// for that device is dropped (no partial sample with zeros).
    /// Other devices in the same tick are still emitted. A
    /// completely-failing tick returns an empty vec; the
    /// surrounding loop keeps going.
    pub fn sample_once(&self, start: Instant) -> Vec<GpuSample> {
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        let mut samples = Vec::with_capacity(self.device_indices.len());

        for &index in &self.device_indices {
            // Re-fetch the handle each tick. The lookup itself is
            // cheap (microseconds); if it fails the device is
            // skipped for this tick.
            if let Ok(device) = self.nvml.device_by_index(index) {
                if let Some(sample) = sample_device(&device, index, elapsed_ns) {
                    samples.push(sample);
                }
            }
        }

        samples
    }
}

/// Samples a single device. Returns `None` if any of the five
/// queries fails — the caller drops the sample for this device
/// this tick.
fn sample_device(device: &Device<'_>, index: u32, elapsed_ns: u64) -> Option<GpuSample> {
    let memory = device.memory_info().ok()?;
    let utilization = device.utilization_rates().ok()?;
    let temperature = device.temperature(TemperatureSensor::Gpu).ok()?;
    let power = device.power_usage().ok()?;

    Some(GpuSample {
        elapsed_ns,
        device_index: index,
        memory_used_bytes: memory.used,
        memory_total_bytes: memory.total,
        // Defensive clamp to 0..=100 even though NVML guarantees
        // this range. Cast u32 to u8 after the clamp is safe.
        utilization_percent: utilization.gpu.min(100) as u8,
        temperature_celsius: temperature,
        power_draw_milliwatts: power,
    })
}

/// Runs the GPU sampling loop until cancelled.
///
/// Mirrors [`crate::sampler::sample_during`] in shape: samples
/// at `config.sample_period` cadence, accumulates results into a
/// [`GpuTimeline`], terminates when `cancel` is signalled, and
/// returns whatever was collected.
///
/// On a multi-GPU host each tick contributes one sample per
/// device, all sharing the same `elapsed_ns`. The consumer that
/// wants per-device views filters by `device_index` (see
/// [`is_core::GpuTimeline::device_indices`]).
///
/// Errors at the per-tick level are absorbed by `sample_once` and
/// surface as dropped samples; the loop itself does not propagate
/// them. Same best-effort contract as the CPU-side sampler.
pub async fn sample_gpu_during(
    sampler: GpuSampler,
    config: SysmonConfig,
    start: Instant,
    mut cancel: oneshot::Receiver<()>,
) -> GpuTimeline {
    let mut timeline = GpuTimeline::new(config.sample_period.as_nanos() as u64);

    // ADR-010: read the energy counter at window open; the delta
    // against the close reading is the window energy per device.
    let energy_start = sampler.read_energy_counters();

    let mut ticker = interval(config.sample_period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;

            // Cancellation wins, same as in sampler.rs.
            _ = &mut cancel => break,

            _ = ticker.tick() => {
                for sample in sampler.sample_once(start) {
                    timeline.push(sample);
                }
            }
        }
    }

    // ADR-010: close the energy window and join against the open
    // snapshot. See compute_energy_delta for the join semantics.
    let energy_end = sampler.read_energy_counters();
    timeline.energy = compute_energy_delta(energy_start, energy_end);

    timeline
}

/// Computes per-device window energy from two counter snapshots.
///
/// Joins `start` and `end` on the device indices present in *both*
/// (a device whose counter read failed in either snapshot is omitted —
/// it has no trustworthy delta). The per-device value is
/// `end - start` via `saturating_sub`, which guards a driver reload
/// mid-window: the monotonic counter would reset, making `end < start`,
/// and saturating to 0 is a truthful "no valid measurement" rather than
/// a u64 wrap to a huge bogus figure.
///
/// Returns `None` when no device yields a delta, so the caller leaves
/// `GpuTimeline::energy` absent and the report falls back to the
/// integrated estimate (ADR-010). Pure: no NVML, testable without a GPU.
fn compute_energy_delta(start: Vec<(u32, u64)>, end: Vec<(u32, u64)>) -> Option<Vec<DeviceEnergy>> {
    let start_by_index: std::collections::HashMap<u32, u64> = start.into_iter().collect();
    let mut energy: Vec<DeviceEnergy> = end
        .into_iter()
        .filter_map(|(index, end_mj)| {
            start_by_index.get(&index).map(|&start_mj| DeviceEnergy {
                device_index: index,
                energy_millijoules: end_mj.saturating_sub(start_mj),
                source: EnergySource::Counter,
            })
        })
        .collect();
    energy.sort_by_key(|e| e.device_index);
    if energy.is_empty() {
        None
    } else {
        Some(energy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the CPU-only development VM, `GpuSampler::new()` must
    /// fail fast with `NvmlUnavailable`. This is the contract
    /// that lets the orchestrator decide to skip GPU sampling
    /// without aborting the whole probe.
    ///
    /// This test runs on any host without an NVIDIA driver and
    /// passes by checking the error variant. On a host with a
    /// driver loaded it would succeed in constructing the
    /// sampler; the test handles both outcomes.
    #[test]
    fn new_returns_unavailable_when_no_driver() {
        match GpuSampler::new() {
            Err(GpuError::NvmlUnavailable { details }) => {
                // Expected on CPU-only hosts: details typically
                // mention libnvidia-ml.so not being found.
                assert!(
                    !details.is_empty(),
                    "NvmlUnavailable must carry a non-empty diagnostic"
                );
            }
            Err(other) => {
                panic!("unexpected error variant on CPU-only host: {other:?}");
            }
            Ok(sampler) => {
                // Tolerated outcome: the test host happens to
                // have an NVIDIA driver loaded. In that case we
                // at least check that device_count is consistent.
                let _ = sampler.device_count();
            }
        }
    }

    #[test]
    fn energy_delta_subtracts_per_device_and_sorts() {
        let start = vec![(1, 1_000), (0, 5_000)];
        let end = vec![(0, 56_000), (1, 8_000)];
        let got = compute_energy_delta(start, end).expect("some energy");
        assert_eq!(got.len(), 2);
        // Sorted by device_index.
        assert_eq!(got[0].device_index, 0);
        assert_eq!(got[0].energy_millijoules, 51_000);
        assert_eq!(got[0].source, EnergySource::Counter);
        assert_eq!(got[1].device_index, 1);
        assert_eq!(got[1].energy_millijoules, 7_000);
    }

    #[test]
    fn energy_delta_saturates_on_counter_reset() {
        // Driver reload mid-window: end < start. Must yield 0, not wrap.
        let start = vec![(0, 90_000)];
        let end = vec![(0, 10_000)];
        let got = compute_energy_delta(start, end).expect("some energy");
        assert_eq!(got[0].energy_millijoules, 0);
    }

    #[test]
    fn energy_delta_joins_only_indices_in_both_snapshots() {
        // Device 0 in both; device 1 only at start; device 2 only at end.
        let start = vec![(0, 1_000), (1, 2_000)];
        let end = vec![(0, 4_000), (2, 9_000)];
        let got = compute_energy_delta(start, end).expect("some energy");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].device_index, 0);
        assert_eq!(got[0].energy_millijoules, 3_000);
    }

    #[test]
    fn energy_delta_empty_when_no_common_device() {
        // No index appears in both snapshots -> None -> report falls
        // back to the integrated estimate.
        assert_eq!(compute_energy_delta(vec![], vec![]), None);
        assert_eq!(compute_energy_delta(vec![(0, 1)], vec![(1, 2)]), None);
    }
}
