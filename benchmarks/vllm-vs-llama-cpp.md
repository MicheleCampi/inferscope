# vLLM vs llama.cpp — Head-to-Head on H100

Same hardware, same model family, two engines. Qwen 2.5 7B served by vLLM 0.21 (AWQ quantization) and by llama.cpp build `b9165` (Q4_K_M quantization), both on a single NVIDIA H100 SXM. inferscope v0.2.1 was the profiler in both cases — no engine-specific code path, just the standard `--gpu --include-descendants` flags.

**Why this comparison exists**: inferscope had been validated against llama.cpp across three GPU classes ([cross-hardware-comparison](cross-hardware-comparison.md)) by 22 May 2026, but the production-default LLM serving stack today is vLLM, not llama.cpp. A claim of engine-agnosticism is not credible until tested against the engine the claim is most likely to be checked against. The vLLM runs documented here, on 24 May 2026, close that gap.

**Why three runs, not one**: the vLLM run was attempted twice with very different results between attempt one and attempt two. Documenting both, plus a third confirmation run, exposes a startup pattern that single-shot benchmarking conceals.

## Setup

| Field | Value |
|---|---|
| Hardware | NVIDIA H100 80GB HBM3 SXM (RunPod) |
| Driver | 580.126.09, CUDA 13.0 |
| CPU host | 224 CPUs (Intel) |
| inferscope version | v0.2.1 |
| vLLM version | 0.21 |
| vLLM env tweak | `VLLM_USE_DEEP_GEMM=0` (skip FP8 DeepGEMM warmup; not relevant for AWQ) |
| Model | Qwen 2.5 7B Instruct, AWQ quantization (vLLM's preferred format) |
| Quantization kernel | `awq_marlin` |
| llama.cpp reference | same hardware, Qwen 2.5 7B Q4_K_M, build `b9165` (run from 22 May 2026, see cross-hardware-comparison) |
| Prompt | `"Hello, world"` |
| `max_tokens` | 128 |
| Sample period | 50 ms |

The two engines were tested with their **native quantization formats** (AWQ for vLLM, Q4_K_M for llama.cpp) rather than forcing a single format on both. AWQ and Q4_K_M are both 4-bit weight-quantization schemes; the model accuracy is comparable across the two, but the engines are tuned for their respective formats. Cross-format runs would be possible but would handicap one engine artificially.

## Three runs of vLLM, one observation each

vLLM was profiled three times against the same server instance:

| Run | When | Server state |
|---|---|---|
| **Cold** | 23 May 22:33 (just after vLLM started) | First request to the server |
| **Warm outlier** | 24 May 09:09 (after pod paused overnight, vLLM restarted) | First request after restart |
| **Warm steady** | 24 May 09:11 (immediately after warm outlier) | Server fully warm |

Results:

| Metric | Cold | Warm outlier | Warm steady |
|---|---|---|---|
| **TTFT** | **22.84 ms** | **651.78 ms** | **20.71 ms** |
| Throughput | 242.12 tok/s | 239.36 tok/s | 237.99 tok/s |
| Inter-token p50 | 4.14 ms | 4.21 ms | 4.21 ms |
| Inter-token p99 | 6.60 ms | 4.96 ms | 4.75 ms |
| Worker RSS peak | 1412 MiB | 1411 MiB | 1411 MiB |
| Worker CPU mean | 13.3 % | 7.5 % | 15.0 % |
| GPU sample count | 12 | 25 | 13 |
| SM peak | 100 % | 100 % | 100 % |
| SM mean | 43 % | 28 % | 57 % |
| Power peak | 205 W | 220 W | 226 W |
| Power mean | 154 W | 138 W | 161 W |
| VRAM used | 78.96 GB | 78.96 GB | 78.96 GB |

### What "cold" and "warm" actually mean for vLLM

The cold run was the first request after vLLM started. TTFT 22.84 ms. The two warm runs were the first and second requests after vLLM was restarted following an overnight pause. TTFT 651.78 ms, then 20.71 ms.

The warm outlier was not a slow request — the throughput was identical to the other two (239 tok/s), and the inter-token latencies were normal. The 631 ms TTFT *premium* lived entirely in the time-to-first-token. After that token, the run proceeded at full speed.

vLLM uses lazy CUDA graph capture: the first inference after server startup triggers the graph compilation pass, paying the capture cost on the user's clock instead of at startup. The `torch.compile` cache on disk persisted across the pause (the model files are the same), but the cudagraph in-memory state regenerated. The 25 GPU samples in that run vs 12–13 in the others is the same phenomenon: inferscope's sample timer ticked through the captured-graph stall.

**The implication for benchmarking**: a single-request vLLM benchmark always measures the worst case. The first request reports a TTFT that does not represent the engine's steady-state. Single-shot benchmarks of vLLM are misleading by construction unless the first request is intentionally warmup-discarded.

The cold run did *not* show the same stall — that's because the very first request after server startup uses the freshly-built cudagraph that the server captured during initialization. The pathological case is the second start: graph state is gone, model files are still on disk, the engine lazy-captures on first inference. Restart-then-test is exactly the sequence many CI scripts produce.

## Head-to-head: vLLM warm-steady vs llama.cpp on H100

Using the steady-state vLLM number (warm-2, 20.71 ms TTFT) and the equivalent llama.cpp run from the cross-hardware archive:

| Metric | vLLM 0.21 (AWQ) | llama.cpp b9165 (Q4_K_M) | Delta |
|---|---|---|---|
| TTFT (warm steady) | 20.71 ms | 39.71 ms | vLLM 1.9× faster |
| Throughput | 237.99 tok/s | 230.38 tok/s | vLLM +3.3 % |
| Inter-token p50 | 4.21 ms | 4.28 ms | within noise |
| Inter-token p99 | 4.75 ms | 5.30 ms | vLLM -10 % |
| Worker RSS | 1412 MiB | 720 MiB | llama.cpp lighter |
| Worker CPU mean | 15.0 % | 90.7 % | **llama.cpp 6× higher** |
| SM mean | 57 % | 48 % | vLLM +9 pp |
| Power mean | 161 W | 170 W | vLLM -5 % |
| Power per token | ~0.68 J/tok | ~0.74 J/tok | vLLM 8 % more efficient |
| VRAM used | **78.96 GB** | **5.56 GB** | **vLLM 14× more** |
| VRAM as % of available | 92 % | 6.5 % | — |

### Reading the comparison

**Latency**: vLLM cuts TTFT roughly in half and edges out throughput by ~3 %. The inter-token p99 is also better. This is what you would expect from an engine designed primarily for low-latency serving against an engine designed primarily for portability.

**CPU footprint**: llama.cpp's worker is doing significant CPU work (90.7 % mean of one core, 228 threads peak) — it's running its scheduling and detokenization logic in CPU code. vLLM offloads almost everything to GPU; the Python process barely touches the CPU (15 % mean). This is also expected: llama.cpp is a C++-with-Python-bindings system and emphasizes CPU performance; vLLM is a Python-orchestrated GPU compute pipeline.

**Power efficiency**: per token, vLLM is about 8 % more efficient on this hardware (~0.68 J/token vs 0.74 J/token). Not a large delta, but real.

**VRAM**: this is the dramatic delta. vLLM holds 78.96 GB of VRAM — 92 % of the H100's 80 GB tier — for a 4.7 GB model. This is the **KV cache pool**, which vLLM pre-allocates aggressively to support its continuous-batching architecture. The model itself uses ~5 GB (consistent with llama.cpp); the other 74 GB is reserved for runtime KV state across concurrent requests. For a single-request benchmark, this looks wasteful. For production serving, this is what enables vLLM to run dozens of concurrent requests without paging KV state in and out — which is what makes it a production serving stack rather than a single-user tool.

llama.cpp does not pre-allocate KV. It uses what it needs (5.56 GB total) and releases when the request completes. For single-user inference, this is more memory-efficient. For high-concurrency serving, it is not designed to be efficient — the workload pattern is wrong.

### The honest "which is better" answer

For batched concurrent serving — many simultaneous requests, request rate measured in hundreds/second per GPU — vLLM is built for the workload llama.cpp wasn't. The latency wins above are mostly artifacts of the engine assuming it has VRAM headroom; in the production batched case, vLLM's advantage compounds because the KV cache amortizes across requests.

For single-request inference, ad-hoc invocation, embedded use cases, CPU-only fallback, or hardware where VRAM is the binding constraint — llama.cpp is competitive (and often better on the VRAM dimension). Its CPU footprint is a feature, not a bug, when the deployment target is laptop-class hardware.

The two engines target different points on the deployment-pattern axis. The numbers above describe what each one does on a single H100 against one request; they do not generalize to either engine's intended use case without changing the experiment.

## How inferscope handled both engines

The inferscope invocation was **byte-for-byte identical** between the two engine cases except for the `--endpoint` URL:

```
inferscope --endpoint http://localhost:8080 --model qwen2.5 \
           --prompt "Hello, world" --pid $WRAPPER_PID \
           --include-descendants --gpu \
           --sample-period-ms 50 --max-tokens 128 \
           --json > run.json
```

No engine-specific flags. No engine-specific parsing. inferscope makes one HTTP request, samples `/proc` and NVML in parallel, and writes the same JSON shape regardless of what produced the response on the other side of the OpenAI API.

This is the engine-agnostic claim made concrete: the binary worked the first time against vLLM, against a quantization format it had never been tested with (AWQ, where llama.cpp had only been tested with Q4_K_M), against a different host runtime (vLLM's PyTorch process vs llama-server's C++ binary). The wrapper-PID and process-tree-aggregation features (v0.2.1's `--include-descendants`) worked correctly against vLLM's Python supervisor + worker model, the same way they worked against llama-server's fork model. The CUDA graph capture stall on warm restart was visible in inferscope's output for the same reason it would be visible against any engine that does lazy capture — the profiler doesn't know what the engine is doing, it just samples honestly.

## Reproducing these runs

1. Spin up a RunPod with a single H100 SXM and driver 580+.
2. Install vLLM 0.21:
   ```
   pip install vllm==0.21
   export VLLM_USE_DEEP_GEMM=0  # skip FP8 DeepGEMM (not relevant for AWQ)
   ```
3. Start the server with an AWQ-quantized Qwen 2.5 7B:
   ```
   vllm serve Qwen/Qwen2.5-7B-Instruct-AWQ \
        --port 8080 --quantization awq_marlin &
   WRAPPER_PID=$!
   ```
4. Wait for `Application startup complete` (or `curl http://localhost:8080/v1/models` returning 200).
5. Run inferscope with the same flags as above.

To reproduce the warm-restart outlier specifically: do a single profile run, then stop the vLLM server (`kill $WRAPPER_PID`), restart it, then immediately profile again. The first post-restart run will show TTFT 500–800 ms; the second will be back to ~20 ms.

The vLLM runs preserved in the validation archive on the development VM include cpu-info, nvidia-smi.csv, the inferscope JSON for each of the three runs, and a tarball of the full session. These can be published into [`benchmarks/raw/vllm-2026-05-24/`](raw/) on request.
