# Cross-Hardware Comparison

The same inferscope binary run against the same llama.cpp build, the same Qwen 2.5 family models, on three different NVIDIA GPU classes. Numbers come directly from inferscope's JSON output (aggregate metrics) and from the per-run summary report (per-device breakdowns). All raw JSON and context files are preserved in the validation archive on the development VM and can be re-published on request.

**inferscope versions**: v0.2.0 (L4 runs, 20 May 2026), v0.2.1 (H100 + 4×A40 runs, 22–23 May 2026).
**llama.cpp version**: build `b9165` (commit `769cc93`).
**Sampling**: `--sample-period-ms 50`, `--include-descendants`, `--gpu`.
**Models**: Qwen 2.5 0.5B / 7B / 32B Q4_K_M (GGUF).
**Decode shape**: `max_tokens 128` (256 for the 32B run).

## A note on aggregate vs per-device numbers

inferscope v0.2.x aggregates GPU metrics across all visible devices: the `gpu` section in the JSON reports cluster-wide max VRAM, mean SM utilization, mean power, etc. This is correct for single-GPU runs and informative for multi-GPU runs where the load is balanced, but it **hides asymmetric workloads** — a tensor-parallel run that touches only two of four GPUs will report a "cluster mean SM" that averages two busy GPUs with two idle ones.

Per-device numbers in the tables below come from the human-readable summary report that reads the raw timeline samples directly. The discrepancy between aggregate and per-device readings on multi-GPU runs is what motivates v0.3 and ADR-007 — see [Workload C](#workload-c--multi-gpu-tensor-parallel-7b-model) for a concrete example.

## Hardware

| Property | L4 (RunPod) | H100 SXM (RunPod) | A40 ×4 (RunPod) |
|---|---|---|---|
| Architecture | Ada Lovelace | Hopper | Ampere |
| Compute capability | 8.9 | 9.0 | 8.6 |
| VRAM per GPU | 24 GB GDDR6 | 80 GB HBM3 | 48 GB GDDR6 |
| TDP per GPU | 72 W | 700 W | 300 W |
| Hourly cost (Community Cloud, May 2026) | ~$0.45/hr | ~$3.29/hr | ~$1.36/hr each (×4 = $5.44/hr) |

The 4×A40 host had two NUMA sockets with two GPUs each. PCIe topology was recorded in the run archive (`topology.txt`). This matters for tensor-parallel splits; see Workload C.

## Workload A — Hardware-proportionate model sizes

Each hardware running the model size it would plausibly serve in production. Cross-hardware comparison is approximate because the model changes across rows; what is being illustrated is what each hardware does with what it would actually be deployed against.

| Metric | L4 + Qwen 0.5B Q4 | H100 + Qwen 7B Q4 | 4×A40 + Qwen 7B Q4 (TP=2) |
|---|---|---|---|
| Model size on disk | 352 MB | 4.7 GB | 4.7 GB |
| TTFT (warm) | 13.12 ms | 39.71 ms | 65.65 ms |
| Throughput | 381.20 tok/s | 230.38 tok/s | 106.33 tok/s |
| Inter-token p50 | 2.62 ms | 4.28 ms | 9.31 ms |
| Inter-token p99 | 2.74 ms | 5.30 ms | 15.99 ms |
| Tokens emitted | 80 | 128 | 128 |
| RSS (worker CPU-side, peak) | 511 MiB | 720 MiB | 765 MiB |
| CPU mean | 75.96 % | 90.72 % | 94.60 % |
| Threads peak | 52 | 228 | 102 |
| VRAM used (single GPU on L4/H100; sum across active GPUs on A40) | 1.34 GB | 5.56 GB | ~5.4 GB (2.56 + 2.87 across GPU 0–1) |
| SM utilization | 58 % mean / 91 % peak | 48 % mean / 90 % peak | GPU 0: 33 % mean / GPU 1: 38 % mean (others idle) |
| Power mean (per active GPU) | 36.8 W | 169.8 W | ~150 W per active GPU |
| Temp peak | 44 °C | 40 °C | 53 °C |

### Reading the table

**L4** with the small model is doing the most proportional work: more than half its power envelope, SM mean 58 %. The chip is engaged.

**H100** with Qwen 7B is loafing. SM mean 48 %, power at 24 % of TDP, VRAM at 6.5 % of capacity. The 7B-Q4 model does not exercise H100; a 30–70B model would (see Workload B).

**4×A40 with tensor-parallel = 2** is the slowest of the three despite costing more per hour than H100 ($5.44/hr aggregated vs $3.29/hr for H100). Throughput is less than half of H100's (106 vs 230 tok/s). Two of the four GPUs sit completely idle — they hold no weights but the cluster is still billed by the hour. The per-device columns expose this; the aggregate JSON does not.

## Workload B — Same hardware, varying model size

H100 alone, Qwen 7B vs Qwen 32B, both Q4_K_M. Isolates the model-size axis.

| Metric | H100 + 7B Q4 | H100 + 32B Q4 | Delta |
|---|---|---|---|
| Model size on disk | 4.7 GB | 18.7 GB | 4.0× |
| TTFT (warm) | 39.71 ms | 87.49 ms | 2.2× |
| Throughput | 230.38 tok/s | 68.74 tok/s | 0.30× |
| Inter-token p50 | 4.28 ms | 14.44 ms | 3.4× |
| Inter-token p99 | 5.30 ms | 19.95 ms | 3.8× |
| RSS (worker) | 720 MiB | 858 MiB | 1.2× |
| CPU mean | 90.72 % | 98.81 % | near-saturation |
| Threads | 228 | 228 | invariant |
| VRAM used | 5.56 GB (6.5 %) | 21.44 GB (25.1 %) | 3.85× |
| SM peak | 90 % | 97 % | +7 pp |
| SM mean | 48 % | 88 % | +40 pp |
| Power peak | 262.3 W | 529.4 W | 2.0× |
| Power mean | 169.8 W | 439.0 W | 2.6× |
| Power / TDP (mean) | 24 % | 63 % | — |
| Temp peak | 40 °C | 44 °C | +4 °C |

### Reading the table

This is where H100 earns its hourly cost. SM mean almost doubles, power mean almost triples, the chip stops loafing. Throughput drops 70 % because compute time per token scales with parameter count; whether 68 tok/s is "enough" is a product decision, not a hardware one. The relevant takeaway for capacity planning: H100's ceiling for Qwen-Q4 inference is somewhere between 7B (loafing) and 32B (busy but still has 40 % power headroom).

## Workload C — Multi-GPU tensor-parallel, 7B model

Same Qwen 2.5 7B Q4 model, served on the 4×A40 host with llama.cpp's `--tensor-split` flag to spread weights across GPUs. Two configurations were tested.

### Aggregate JSON readings

| Metric | TP=2, single socket | TP=4, cross-socket | Single H100 (reference) |
|---|---|---|---|
| TTFT (warm) | 65.65 ms | 61.93 ms | 39.71 ms |
| Throughput | 106.33 tok/s | 105.77 tok/s | 230.38 tok/s |
| Inter-token p50 | 9.31 ms | 9.39 ms | 4.28 ms |
| Inter-token p99 | 15.99 ms | 13.14 ms | 5.30 ms |
| RSS (worker) peak | 765 MiB | 949 MiB | 720 MiB |
| CPU mean | 94.6 % | 97.7 % | 90.7 % |
| VRAM max (aggregate) | 3.08 GB | 2.06 GB | 5.56 GB |
| SM peak (aggregate) | 50 % | 29 % | 90 % |
| GPU sample count | 108 | 108 | comparable |
| Device count (visible) | 4 | 4 | 1 |

### Per-device breakdown (from summary report)

| | TP=2 single socket | | | | TP=4 cross-socket | | | |
|---|---|---|---|---|---|---|---|---|
| | **GPU 0** | **GPU 1** | **GPU 2** | **GPU 3** | **GPU 0** | **GPU 1** | **GPU 2** | **GPU 3** |
| VRAM used | 2.56 GB | 2.87 GB | 0.00 GB | 0.00 GB | 1.55 GB | 1.85 GB | 1.69 GB | 1.74 GB |
| SM mean | 33.1 % | 37.5 % | 0.0 % | 0.0 % | ~17 % across all four (estimated from aggregate) | | | |
| Power mean | 148.0 W | 152.5 W | 34.1 W (idle) | 32.3 W (idle) | ~64 W per GPU (aggregate-derived) | | | |
| Temp max | 52 °C | 53 °C | 33 °C | 34 °C | comparable across all four | | | |

### Reading the tables

**TP=2 vs TP=4 throughput is essentially identical** (106.3 vs 105.8 tok/s). This was not the result the test was designed to find — the working hypothesis was that cross-socket TP=4 would pay a measurable latency penalty for cross-NUMA communication. The data does not support that hypothesis for a 7B model. The latency-per-token distributions are statistically indistinguishable.

**TP=2 leaves two GPUs at idle** (34 W each, the A40 idle floor). They are billed by the hour regardless. For a 7B model that fits in single-GPU VRAM, TP=2 on a 4-GPU node is an expensive way to run a single-GPU workload.

**Why the aggregate JSON understates the situation**: cluster-wide VRAM max of 3.08 GB on a TP=2 run looks like "the model fits comfortably in 1.6 % of available VRAM" until you notice that the 3.08 GB is split across GPU 0 (2.56 GB) and GPU 1 (2.87 GB), and that the aggregate "SM peak 50 %" is the momentary peak of *one* device, not the cluster average. The cluster average is closer to 18 % because GPU 2 and 3 contribute zero. This is the gap that ADR-007 and v0.3's per-device `GpuMetrics` are intended to close: an operator looking at the aggregate JSON should not have to cross-reference a separate summary to understand the actual workload shape.

**vs single H100**: TP=2 and TP=4 on 4×A40 both deliver under half the throughput of single H100 ($5.44/hr vs $3.29/hr), with worse TTFT and ~2× the inter-token latency. Multi-GPU on Ampere does not match single-GPU on Hopper for this workload class.

## Wrapper-PID bug, cross-hardware reproduction

The wrapper-PID failure (parent supervisor reports near-zero RSS / CPU / threads when the engine forks workers) appears identically on every hardware tested. The fix is the v0.2.1 `--include-descendants` flag.

| Metric | L4 wrapper bug | H100 wrapper bug | Real worker (L4) | Real worker (H100) |
|---|---|---|---|---|
| RSS reported | 2.14 MiB | 2.25 MiB | 511.34 MiB | 720 MiB |
| RSS undercount | 239× | 320× | — | — |
| Threads reported | 1 | 1 | 52 | 228 |
| Thread undercount | 52× | 228× | — | — |
| CPU reported | 0 % | 0 % | 75.96 % | 90.72 % |

The undercount magnitude grows with workload size: the larger the real worker, the more dramatic the wrapper-PID misreading. On H100 + 7B, sysmon without `--include-descendants` sees 0.3 % of the real RSS and 0.4 % of the real thread count.

### Fix validation on H100

Same inferscope command, same `llama-server` target, two runs:

| Metric | `--pid` worker (ground truth) | `--pid` wrapper + `--include-descendants` | Delta |
|---|---|---|---|
| RSS peak | 719.98 MiB | 722.41 MiB | +0.3 % (bash overhead) |
| Threads peak | 228 | 229 | +1 (bash itself) |
| CPU mean | 90.72 % | 90.77 % | +0.05 % |
| stderr warning | none | none | — |

The fix produces values statistically indistinguishable from the ground-truth case. The +0.3 % delta is the bash wrapper itself, which is the correct accounting: the fix sums, it does not impersonate.

## Reproducing these numbers

1. Spin up a RunPod container matching the hardware row (image: `nvidia/cuda:13.0.2-runtime-ubuntu22.04` or the official llama.cpp image).
2. Install `inferscope` v0.2.1: `cargo install --git https://github.com/MicheleCampi/inferscope --tag v0.2.1 --features gpu-nvidia`. Alternative: pull the Docker image once published.
3. Build llama.cpp from commit `769cc93` (tag `b9165`).
4. Download the Qwen 2.5 GGUF file matching the size column.
5. Start `llama-server` and capture both the wrapper PID and the worker PID. For tensor-parallel runs, pass `--tensor-split` to `llama-server` with appropriate weights.
6. Run inferscope: `inferscope --endpoint http://localhost:8080 --model qwen2.5 --prompt "Hello, world" --pid <wrapper-pid> --include-descendants --gpu --json > run.json`.

The numbers above were captured with `max_tokens 128` (256 for the 32B run). Increasing this changes throughput slightly (warmup vs steady-state mix) but not by more than a few percent.

The per-device breakdowns rely on inferscope's text summary, not the JSON aggregate fields. v0.3 will surface per-device metrics in the JSON directly; until then, the summary is authoritative for multi-GPU runs.
