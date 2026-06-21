# Validation Results

Evidence artifacts from hardware validation runs.

## adr-010-a10-energy-counter.json

End-to-end validation of the ADR-010 energy/efficiency path on real NVIDIA
hardware. Captured on an NVIDIA A10 (driver 580.105.08) running inferscope
against a llama.cpp server (gemma-3-1b-it, full GPU offload), 128 real tokens
generated.

Confirms:
- `efficiency` block present in JSON report
- `energy_source: "counter"` — NVML `nvmlDeviceGetTotalEnergyConsumption`,
  not the trapezoidal integral fallback
- `tokens_per_joule` numerically sane (128 tokens / 79.3 J)

This closes the only non-cold-path part of ADR-010: prior validation covered
the counter in isolation (Phase 0, A10) and unit-level delta/derivation logic;
this run exercises the full pipeline with real NVML + real token accounting.
