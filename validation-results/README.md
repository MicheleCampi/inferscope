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

## multigpu-a40-2026-05-23/

Multi-device NVML sampling validated on 4× NVIDIA A40 (RunPod, driver
565.57.01, 300 W power limit per card), llama.cpp b9165 with
`--tensor-split`, Qwen 2.5 7B Q4_K_M, inferscope v0.2.1 with
`--include-descendants --gpu`, 50 ms tick.

Two arms, one 128-token request each: `tp2-multigpu.json`
(`CUDA_VISIBLE_DEVICES=0,1`, same-socket PXB pair) and
`tp4-crosssocket.json` (all four devices, crossing the NUMA boundary —
see `topology.txt`).

Confirms:
- all four devices enumerated, one sample per device per tick
  (`sample_count: 108` = 27 ticks × 4 devices), each tagged `device_index`
- boundary-device asymmetry under `--tensor-split 1,1,1,1`: first and last
  device carry more VRAM and SM occupancy than the middle pair
- idle power floor on unused devices: 34.1 W and 32.3 W with zero compute

Caveat, load-bearing for anything citing these numbers: **n=1 per arm**,
~1.26 s of wall time each. No variance estimate. The TP=2 vs TP=4
throughput delta (−0.5%) is not distinguishable from run-to-run noise in
either direction.

This is the evidence behind the multi-GPU article and the motivating data
for ADR-007 (per-device GPU metrics), which shipped in v0.3.0. Every
per-device figure was recovered by post-processing `gpu_timeline.samples[]`
by hand — the v0.2.1 aggregates could not express it.

## vllm-h100-2026-05-24/

inferscope against vLLM 0.21 (Qwen 2.5 7B AWQ, awq_marlin) on 1× H100 SXM,
unchanged code path — confirms the profiler is not llama.cpp-specific.
Three 128-token runs, plus the two server logs.

Confirms:
- vLLM vs llama.cpp on identical hardware, comparing single coherent runs
  (`vllm-h100-warm2.json` vs `../../../inferscope-validation-h100/a2-correct-pid.json`):
  TTFT 20.71 ms vs 39.71 ms, throughput 237.99 vs 230.38 tok/s,
  VRAM 73.53 GiB vs 5.17 GiB
- first-inference JIT cost, see below

**Correction to `vllm-summary.txt` (recorded 2026-07-25, file left intact).**
The COMPARISON section of that summary claims "WARM2 reference" but reports
SM mean 43% (which is the COLD run) and power mean 138 W (which is WARM1).
Its "-19% on power per token" figure pairs 138 W from one run with 238 tok/s
from another and should not be cited. Computed from coherent runs, energy per
token differs by −8.0% using aggregate power and −15.5% using only
active-GPU samples — two defensible estimators disagreeing by nearly 2× over
13 GPU samples per run. There is no usable per-token energy result in this
dataset; ADR-010's NVML energy counters exist because of this.

**Startup cost — what the server logs actually show.** The three TTFT values
are 22.84 ms (`cold`), 651.78 ms (`warm`), 20.71 ms (`warm2`). The labels are
misleading: `vllm-server.log` records two POSTs, and `test-context.txt` dates
the profiled COLD run to ~22:33, i.e. the *second* one. The spike is not
CUDA graph capture (finished in 3 s during init, before the server accepted
traffic) and not torch.compile (15.93 s cold vs 16.62 s on restart —
recompiled either way, no cache shortcut). vLLM names the cause itself, in
both logs, on the first inference after each engine start:
`jit_monitor.py:103 — Triton kernel JIT compilation during inference:
_compute_slot_mapping_kernel. This causes a latency spike`.

Operationally: the first request after any engine start pays a JIT
compilation cost. A benchmark that sends a throwaway request first will never
see it — which is exactly what happened to the run labelled COLD here.
