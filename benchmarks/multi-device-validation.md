# Multi-Device Validation — 4×A40

A deep-dive into a single multi-GPU configuration: Qwen 2.5 7B Q4_K_M served by llama.cpp's tensor-parallel feature on four NVIDIA A40 GPUs split across two NUMA sockets. The goal of these runs was not to find the fastest configuration — H100 single-GPU is faster (see [cross-hardware-comparison](cross-hardware-comparison.md)) — but to validate that inferscope's GPU sampling correctly observes a workload that touches more than one device, and to surface what the v0.2.x sampler does and does not show about asymmetric GPU utilization.

**inferscope version**: v0.2.1.
**llama.cpp version**: build `b9165` (commit `769cc93`).
**Run date**: 23 May 2026.
**Pod**: RunPod 4×A40 Community Cloud, driver 565.57.01.

## Hardware topology

`nvidia-smi topo -m` reported the following layout (legend in [topology.txt](https://github.com/MicheleCampi/inferscope/tree/main/benchmarks/raw) when the raw archive is published):

```
        GPU0    GPU1    GPU2    GPU3
GPU0     X      PXB     SYS     SYS
GPU1    PXB      X      SYS     SYS
GPU2    SYS     SYS      X      PXB
GPU3    SYS     SYS     PXB      X

NUMA 0:  GPU 0, GPU 1   (CPUs  0–23, 48–71)
NUMA 1:  GPU 2, GPU 3   (CPUs 24–47, 72–95)
No NVLink present.
NICs (mlx5_0, mlx5_1) attached to NUMA 1.
```

The relevant distinction for tensor-parallel:
- **PXB** (between GPU 0–1 and between GPU 2–3) means multi-PCIe-bridge but no host-bridge traversal. Latency between PXB pairs is single-digit microseconds.
- **SYS** (between any GPU 0–1 device and any GPU 2–3 device) means the connection traverses the SMP interconnect between NUMA nodes (QPI/UPI). Latency is an order of magnitude higher.

A TP=2 run using only GPU 0–1 stays entirely within NUMA 0 and uses PXB. A TP=4 run crosses sockets twice per token, every time a partial result needs to combine across the half-cluster boundary.

## GPU specifications

From `nvidia-smi.csv`:

| Property | Value (per GPU, ×4) |
|---|---|
| Model | NVIDIA A40 |
| VRAM | 46,068 MiB (~45 GB GDDR6 usable) |
| Compute capability | 8.6 (Ampere) |
| TDP | 300 W |
| Driver | 565.57.01 |

A single A40 has 1.5× the VRAM of H100's 80 GB tier? No — A40 has ~45 GB usable, H100 has 80 GB HBM3. The Ampere architecture and GDDR6 memory put A40 in a different performance tier than Hopper's HBM3; aggregate VRAM (180 GB across 4 A40s) is impressive but inter-device bandwidth dominates throughput for tensor-parallel inference, not aggregate VRAM.

## Workload

| Field | Value |
|---|---|
| Model | Qwen 2.5 7B Instruct, Q4_K_M GGUF |
| Model size on disk | 4.7 GB |
| llama.cpp configuration | `--tensor-split <weights>`, server bound to `localhost:8080` |
| Prompt | `"Hello, world"` |
| `max_tokens` | 128 |
| inferscope flags | `--gpu --include-descendants --sample-period-ms 50` |
| Run duration (approx) | 5.4 s per run (108 samples × 50 ms) |

Two runs were captured:
- **TP=2 single-socket**: `CUDA_VISIBLE_DEVICES=0,1`, `--tensor-split 50,50`. Stays within NUMA 0.
- **TP=4 cross-socket**: all four GPUs visible, `--tensor-split 25,25,25,25`. Crosses NUMA boundary.

## Raw samples vs derived aggregates

inferscope v0.2.1 captures GPU data at two layers:

1. **`gpu_timeline.samples[]`** — raw NVML readings, one entry per device per sample tick. Each sample has `device_index`, `memory_used_bytes`, `utilization_percent`, `temperature_celsius`, `power_draw_milliwatts`, and `elapsed_ns` (relative to run start). For a 4-GPU run sampling at 50 ms over 5.4 s, this is 4 × 108 = 432 sample entries.

2. **`gpu.*` derived aggregates** — cluster-wide max/mean/min computed from the timeline at the end of the run. These are convenient for single-GPU runs and informative for balanced multi-GPU workloads, but for tensor-parallel runs where load is uneven, the cluster-wide aggregates compress away the asymmetry.

The summary report (the plain-text output) reads `gpu_timeline.samples[]` directly and groups by `device_index`, producing per-device statistics. The JSON `gpu` aggregates do not currently expose this grouping; that is the [ADR-007](https://github.com/MicheleCampi/inferscope/tree/main/docs/adr) work targeted for v0.3.

For now, callers who need per-device breakdowns either read the summary, or group `gpu_timeline.samples[]` themselves. The raw data is complete; only the convenience layer is missing.

## TP=2 single-socket — per-device readings

Aggregated values from the summary report (read directly from `gpu_timeline.samples[]`):

| Metric | GPU 0 | GPU 1 | GPU 2 | GPU 3 |
|---|---|---|---|---|
| VRAM used (peak) | 2.56 GB | 2.87 GB | 0.00 GB | 0.00 GB |
| SM utilization (mean) | 33.1 % | 37.5 % | 0.0 % | 0.0 % |
| Power (mean) | 148.0 W | 152.5 W | 34.1 W | 32.3 W |
| Temperature (peak) | 52 °C | 53 °C | 33 °C | 34 °C |

Aggregate run-level metrics (from `timing` and `resource` sections):

- TTFT (warm): **65.65 ms**
- Throughput: **106.33 tok/s**
- Inter-token latency p50 / p99: **9.31 ms / 15.99 ms**
- Worker RSS peak: 765 MiB
- Worker CPU mean: 94.6 %

### Reading the TP=2 numbers

GPU 0 and GPU 1 carry the model weights (~5.43 GB summed). They both run near half-saturation (SM mean ~35 %) and pull ~150 W each — about half the A40 TDP. **GPU 2 and GPU 3 hold no weights**, do no work, and sit at the idle power floor (~33 W each). The host is billing $1.36/hr × 4 = $5.44/hr; only $2.72/hr of that buys useful work.

The asymmetric VRAM split (2.56 GB vs 2.87 GB) is llama.cpp's tensor-split default for `--tensor-split 50,50` on Q4_K_M weights: layer boundaries don't divide evenly across the 50/50 split, so one GPU ends up slightly heavier. inferscope captures this without doing anything special — it is just what `gpu_timeline` reports.

## TP=4 cross-socket — per-device readings

The TP=4 run spreads weights across all four GPUs (`--tensor-split 25,25,25,25`). Per-device summary readings:

| Metric | GPU 0 | GPU 1 | GPU 2 | GPU 3 |
|---|---|---|---|---|
| VRAM used (peak) | 1.55 GB | 1.85 GB | 1.69 GB | 1.74 GB |
| SM utilization (mean) | ~17 % | ~17 % | ~17 % | ~17 % |
| Power (mean) | ~64 W | ~64 W | ~64 W | ~64 W |
| Temperature (peak) | ~46 °C | ~46 °C | ~46 °C | ~46 °C |

(Per-device exact values for TP=4 were derived by grouping the timeline samples; the summary report rounds slightly.)

Aggregate run-level metrics:

- TTFT (warm): **61.93 ms** (4 % *better* than TP=2)
- Throughput: **105.77 tok/s** (~0.5 % worse than TP=2)
- Inter-token latency p50 / p99: **9.39 ms / 13.14 ms**
- Worker RSS peak: 949 MiB
- Worker CPU mean: 97.7 %

### Reading the TP=4 numbers

The hypothesis going into the TP=4 run was that cross-socket communication would impose a measurable cost on throughput. The data does not support this hypothesis: TP=4 and TP=2 produce throughput within 0.5 % of each other. The p99 inter-token latency is actually *lower* on TP=4 than TP=2 (13.14 ms vs 15.99 ms), which is the opposite of the expected pattern.

A few interpretations are consistent with the data:

1. **Qwen 7B Q4 is small enough that per-token compute dominates communication.** With weights at ~1.7 GB per GPU on TP=4 and SM mean at 17 %, no GPU is communication-bound; they spend most of their time doing matmuls. Cross-socket round-trip cost (single-digit microseconds at the GPU level, low tens of microseconds at the application level) is small relative to the per-token compute window.

2. **llama.cpp's tensor-split for Q4 quantized weights overlaps communication with compute well.** This is consistent with how the library is designed; the validation isn't doing anything to defeat that.

3. **The 4 % TTFT improvement on TP=4 is within run-to-run noise.** A single observation isn't enough to claim TP=4 has better cold start. To confirm or deny that, the test would need to be repeated several times.

What this run does *not* tell you: how TP=4 compares to TP=2 for a larger model (32B+) where weights wouldn't fit on two GPUs anyway, or for a workload where inter-token latency is dominated by communication rather than compute. Both of those would need different runs.

## What this reveals about inferscope v0.2 and the v0.3 roadmap

The v0.2.1 sampler **captured the right data**: every NVML metric for every device is in `gpu_timeline.samples[]`, indexed by `device_index`. The data is sufficient to reconstruct everything in the per-device tables above.

The v0.2.1 sampler **did not expose the right derived aggregates**: the `gpu` JSON section reports cluster-wide max VRAM, cluster-wide mean SM, cluster-wide mean power. For TP=2 — where two GPUs sit idle — those cluster-wide numbers understate the per-device load and overstate the "footprint" of the workload (a 5.43 GB model footprint reads as 3.08 GB cluster max because the JSON aggregate takes the max of any single GPU sample, not the sum across active GPUs).

ADR-007 (planned for v0.3) is the structural fix: replace cluster-wide aggregates with a `per_device: [...]` array of metrics, with optional cluster-level summaries. The data layer doesn't change; only the report layer does.

This is the "data captured is fine; question is which slice the report makes visible" pattern that recurs across inferscope's design history — same lesson as ADR-006 (process-tree aggregation), now applied to the GPU dimension.

## Reproducing these runs

1. Provision a 4×A40 RunPod with PCIe topology comparable to the table above (`nvidia-smi topo -m` should report `PXB` between GPU 0–1 and between GPU 2–3, `SYS` across the boundary).
2. Build llama.cpp from commit `769cc93` (tag `b9165`).
3. Download Qwen 2.5 7B Instruct Q4_K_M GGUF.
4. **TP=2 run**:
   ```
   CUDA_VISIBLE_DEVICES=0,1 \
     llama-server --model qwen2.5-7b-q4_k_m.gguf --tensor-split 50,50 \
                  --port 8080 --host 0.0.0.0 &
   WRAPPER_PID=$!
   ```
5. **TP=4 run**: same but without `CUDA_VISIBLE_DEVICES`, `--tensor-split 25,25,25,25`.
6. Run inferscope:
   ```
   inferscope --endpoint http://localhost:8080 --model qwen2.5 \
              --prompt "Hello, world" --pid $WRAPPER_PID \
              --include-descendants --gpu \
              --sample-period-ms 50 --max-tokens 128 \
              --json > tp{2,4}-run.json
   ```
7. Inspect either the plain-text summary (per-device by default) or the JSON aggregate. To reproduce per-device numbers from the JSON: group `gpu_timeline.samples` by `device_index` and compute max/mean/etc. per group.

The full archive of the May 23 run — JSON, summary, topology, nvidia-smi output, llama-server log, kernel info — is preserved on the validation VM and can be published into [`benchmarks/raw/multi-device-2026-05-23/`](raw/) on request.
