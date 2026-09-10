# ADR-014 — live SGLang scrape, A10

2026-09-10 · NVIDIA A10 23GB, driver 580.105.08 · SGLang 0.5.19 ·
Qwen2.5-0.5B-Instruct · inferscope at `92f9405`

Closes the gap ADR-014 and the project README had carried since August:
*"a live scrape against a running SGLang server needs a GPU and has not
been done."* It has now.

## Result

`sglang-run-3.json` carries a derived KV-cache section:

| | |
|---|---|
| hit rate | **0.9859** |
| hits / queries over the window | 420 / 426 |
| accounting | `exact_tokens` |
| samples | 10 at 250 ms |
| window energy | 280.39 J |

`exact_tokens` is correct because the server reports `'page_size': 1` in
its own startup args (`server-startup.log`), which is what SGLang resolves
to on a non-MLA model. The overrides to 64 and 128 apply to FlashMLA,
Cutlass MLA and TensorRT-LLM MLA only.

## What this confirms

**The `is_streaming` fix, on a real endpoint.** `exposition.txt` was
captured after driving the server with two clients: `curl`, which does not
stream, and inferscope's probe, which always does. Both token counters
came back split across the label:

| series | `is_streaming="false"` | `is_streaming="true"` | true total |
|---|---|---|---|
| `sglang:prompt_tokens_total` | 574 | 213 | **787** |
| `sglang:generation_tokens_total` | 216 | 1024 | **1240** |

Under the single-line aggregation this schema used until 2026-09-05, the
parser would have returned 574 or 213 for the first and 216 or 1024 for
the second, depending on exposition order — errors of 27% and 83% against
the true totals. The fix was made by re-reading the collector at source,
before this run existed; this is the first time the split has been
observed rather than reasoned about.

**The numerator is per-source.** `cached_tokens_total` comes back under
`cache_source="device"`, not the `total` fallback, so summing every source
except the reserved `total` reads the real figure.

## What the session caught

**SGLang serves `/metrics` only with `--enable-metrics`.** Without it the
endpoint is a plain 404 — not an empty body, not a body missing those
series. Nothing in ADR-014 or the project README said so, and it is the
first thing anyone pointing inferscope at SGLang will hit.

**`ninja` must be on PATH, and this is not a vLLM quirk.** FlashInfer
compiles kernels at engine start and invokes `ninja` as an executable, so
pip-installing it into the venv is not enough. The same failure appeared
on vLLM during the ADR-016 pilot and was recorded there as a vLLM
environment note; it is a FlashInfer property and applies to any engine
using it.

## The shape of the measurement, and its limit

The first two runs carry a KV timeline with four samples and **no derived
section**. That is correct rather than a failure: SGLang updates these
counters when a request finishes, and the probe's window closes with its
own request. Every sample inside one window reads the same value,
`queries_delta` is zero, and `derive_kvcache` returns `None` instead of a
rate over nothing.

`sglang-run-3.json` was driven differently — six short requests in
parallel while the probe ran one long one. The short ones complete inside
the window, so the counters move.

That is a property of window-differenced counters against an engine that
publishes on completion, and it belongs in the runbook: **a single-request
probe cannot derive a hit rate from an engine that only reports when a
request ends.**

## Files

| file | what it is |
|---|---|
| `sglang-run-1.json` | first probe, cold — no derived KV section |
| `sglang-run-2.json` | second probe, same prompt — still no delta |
| `sglang-run-3.json` | probe under concurrent load — the derived result |
| `exposition.txt` | `/metrics` after all runs, both `is_streaming` values present |
| `server-startup.log` | SGLang's own `server_args`: `page_size: 1`, `enable_metrics: True` |
