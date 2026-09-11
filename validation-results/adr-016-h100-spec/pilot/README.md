# Pilot run — 2026-09-03

One point, `L3.00-minvar`, on the H100 PCIe. Not campaign data: this run
existed to confirm the knob takes effect and to measure what the environment
costs.

## Result

The knob takes effect exactly.

    window deltas:  drafts=5100  draft_tokens=25500  accepted=10200
    realized L   =  1 + 10200/5100 = 3.0000   (configured: 3.0)
    accept rate  =  10200/25500 = 0.4000

Three things this confirms that were previously read but not observed:

- **The `+1` convention (ADR-016 D4) is right.** Without it the figure would
  be 2.0, and every point of the sweep would sit one token below the knob that
  produced it.
- **The draft model produces full k-token proposals.** `draft_tokens/drafts`
  is exactly 5, so no under-production. This is why `draft_model` was chosen
  over ngram, and the ratio confirms the reasoning rather than assuming it.
- **The min-variance schedule behaves as `_acceptance_length_to_rates`
  describes.** The vector `[1.0, 1.0, 0, 0, 0]` yields an acceptance rate of
  exactly 0.4 = 2/5: the first two positions always accepted, the rest never.

## Environment, and what it cost

The Lambda image ships a scientific Python stack built against numpy 1.x.
vLLM installs numpy 2.x, which breaks scipy and sklearn at import time and
kills the engine before it starts. The fix is a clean venv (`python3 -m venv`,
no system site packages), plus `ninja`, which FlashInfer invokes as an
executable from PATH when it compiles kernels at engine start.

Two corrections from the SGLang session of 2026-09-10, where the same
failure appeared on a different engine. It is not a vLLM property: it is
FlashInfer's, so any engine using it behaves the same way — SGLang
included. And pip-installing `ninja` into the venv is not sufficient on
its own: the venv's `bin` has to be on PATH when the engine starts, or
the subprocess lookup still fails with `FileNotFoundError: 'ninja'`.

For the next session, in order:

    python3 -m venv ~/venv
    ~/venv/bin/pip install vllm ninja
    export PATH="$HOME/venv/bin:$PATH"     # FlashInfer looks here
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    cargo build --release --features gpu-nvidia

inferscope must be built on the GPU host: the campaign needs NVML, and a
binary built without `--features gpu-nvidia` reports no energy at all.

## Load note

The generator was asked for 4 req/s and delivered 3.29. Stage one should ask
for 3 to stay inside a genuinely steady regime rather than at the edge of what
this configuration sustains.
