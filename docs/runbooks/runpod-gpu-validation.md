# RunPod GPU validation runbook

This runbook documents the procedure for validating inferscope's
NVIDIA GPU sampling path on real hardware via [RunPod](https://runpod.io).
It is meant to be followed end-to-end in a single session, with
a real budget cost of approximately **\$1–2 per validation run**
on RTX A5000 (24 GB VRAM, Ampere generation, $0.27/hr at the
time of writing).

## When to follow this runbook

- Validating a new version of inferscope's NVIDIA sampling code
  against a real GPU (the development VM cannot test NVML).
- Producing screenshots and JSON snapshots for the v0.2 release
  article and README.
- Reproducing a reported issue against the exact same hardware
  used in prior validation.

## Prerequisites

- RunPod account with at least \$2 of credit (one validation run
  on RTX A5000 takes 30–90 minutes wall time at $0.27/hr).
- SSH key pair on the workstation you will operate from. The
  public key must be registered under RunPod → Settings →
  SSH Public Keys.
- `gpu/v0.2` (or later) branch of inferscope pushed to GitHub.
  The runbook assumes the pod will clone the public repository.

## Step 1 — Spin up a pod

1. RunPod console → Pods → Deploy.
2. Filter by **NVIDIA → Previous Generation → RTX A5000**
   (or current first-test GPU; see top of file for rationale).
3. Choose a pod template that ships with the NVIDIA driver and
   CUDA preinstalled. Recommended:
   `runpod/pytorch:2.x.x-cuda12.x-devel-ubuntu22.04` (or the
   most recent PyTorch / CUDA / Ubuntu combination).
   The CUDA devel image is required so we get the NVIDIA driver
   stack and `nvidia-smi`; the PyTorch part is not used here
   but is the lightest preconfigured template.
4. Storage: defaults are fine. We do not need persistent volumes
   for a validation run.
5. Click Deploy. Wait 30–60 seconds for the pod to become
   `Running`.

## Step 2 — Connect via SSH

Once the pod is `Running`, RunPod shows a command like:
`ssh root@<pod-ip> -p <port> -i ~/.ssh/runpod_optimdev`.

On the operator workstation (the Hetzner dev VM):

    ssh root@<pod-ip> -p <port> -i ~/.ssh/runpod_optimdev

Confirm the host key on first connection.

## Step 3 — Verify the GPU is visible

Inside the pod:

    nvidia-smi

Expected: a table showing the RTX A5000 (or chosen GPU), driver
version, CUDA version, VRAM total, VRAM used, temperature,
power draw. If `nvidia-smi` errors with "command not found" or
"NVIDIA-SMI has failed", the pod template did not include the
driver — destroy the pod and choose a different template.

## Step 4 — Install Rust toolchain

The pod image does not ship Rust. Install it:

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source $HOME/.cargo/env
    rustc --version

Expected: `rustc 1.85.x` or later. Older versions fail because
inferscope pins MSRV 1.85 in `rust-toolchain.toml`.

## Step 5 — Clone and build inferscope

    cd /workspace
    git clone https://github.com/MicheleCampi/inferscope.git
    cd inferscope
    git checkout gpu/v0.2
    cargo build --release --features gpu-nvidia

Build takes 4–7 minutes on an A5000 pod. Verify clean build with
no warnings:

    RUSTFLAGS="-D warnings" cargo check --release --features gpu-nvidia

Expected: clean exit. If warnings appear, the validation is
already a failure — fix on the dev VM and re-push before
continuing.

## Step 6 — Install a small LLM and serve it via llama.cpp

Inferscope needs a real LLM inference workload to exercise the
GPU. The smallest reasonable workload is Qwen 2.5 0.5B Q4
(~500 MB on disk, ~700 MB VRAM):

    apt-get update && apt-get install -y wget build-essential cmake

    cd /workspace
    git clone https://github.com/ggerganov/llama.cpp.git
    cd llama.cpp
    cmake -B build -DGGML_CUDA=ON
    cmake --build build --config Release -j

    # Download the model
    mkdir -p /workspace/models
    wget -O /workspace/models/qwen-0.5b.gguf \
      https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf

    # Start the server (in background)
    ./build/bin/llama-server \
      -m /workspace/models/qwen-0.5b.gguf \
      --port 8080 \
      --n-gpu-layers 99 &

    # Confirm it is up
    sleep 3
    curl -s http://localhost:8080/v1/models

Expected: JSON response listing the loaded model. If the curl
hangs, the server failed to start; check its stderr.

### Finding the correct server PID

**Do not use `$!` from the background launch.** Bash returns the
PID of a transient wrapper shell that handles redirection, not
the `llama-server` worker. If you use `$!`, inferscope will
faithfully sample a process with RSS ~2 MiB, 0 jiffies, and 1
thread — completely uncorrelated with the actual inference
workload.

The correct pattern is:

    SERVER_PID=$(pgrep -x llama-server | head -1)
    echo "Server PID: $SERVER_PID"

`pgrep -x` matches the exact process name (not the command line),
returning the long-lived worker rather than any wrapper.

**Sanity check before running inferscope**: a healthy worker
should have RSS in the hundreds of MiB at minimum (the model
weights live there, even with GPU offload some host-side data
remains) and threads above 1:

    cat /proc/$SERVER_PID/status | grep -E "VmRSS|Threads"

If RSS is below 10 MiB or Threads is exactly 1, the PID is wrong —
re-run `pgrep -x llama-server` and pick another candidate.


## Step 7 — Run inferscope with --gpu

    cd /workspace/inferscope
    ./target/release/inferscope \
      --endpoint http://127.0.0.1:8080 \
      --model qwen \
      --prompt "Write three short sentences about Italian coffee." \
      --max-tokens 80 \
      --pid $SERVER_PID \
      --gpu

Expected output: probe summary, inter-token latency, process
resource usage, **and** GPU resource usage. A GPU section
that says something like:

    GPU resource usage (NN samples)
      VRAM               peak X.X GiB  mean X.X GiB
                         min  X.X GiB  total 24.0 GiB
      SM utilization     peak NN%  mean NN%  min NN%
      Temperature        peak NN C
      Power draw         peak NN.N W  mean NN.N W

If the GPU section is absent or the warning
`GPU sampling requested but unavailable: NVML unavailable`
appears, the test has uncovered a real bug — capture full
output and stop here.

## Step 8 — Capture artefacts

For each validation run, capture:

- **Text output**: redirect to a file.
- **JSON output**: re-run with `--json`, redirect to a file.
- **nvidia-smi snapshot**: `nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv > nvidia-smi.csv`
- **Pod environment**: `cat /etc/os-release > os-release.txt` and `uname -a > uname.txt`

Bundle and download:

    mkdir -p /workspace/validation
    cd /workspace/validation
    ./target/release/inferscope --endpoint ... --pid $SERVER_PID --gpu > text.out 2>&1
    ./target/release/inferscope --endpoint ... --pid $SERVER_PID --gpu --json > json.out 2>&1
    nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv > nvidia-smi.csv
    cat /etc/os-release > os-release.txt
    uname -a > uname.txt
    tar czf validation-$(date +%Y-%m-%d).tar.gz *.out *.csv *.txt

From the operator workstation:

    scp -P <port> -i ~/.ssh/runpod_optimdev \
      root@<pod-ip>:/workspace/validation/validation-*.tar.gz ./

## Step 9 — Tear down the pod

**Important — RunPod charges per minute regardless of whether
the GPU is in use.** A forgotten running pod costs $0.27/hr
($6.48/day) silently.

When validation is complete:

1. RunPod console → Pods → click the pod → Stop or Terminate.
2. Verify on the Billing page that current spend rate dropped
   to $0.00/hr.

Use Stop if you plan to resume work in the next hour
(persistent storage charges still apply at a lower rate).
Use Terminate if you are done — this fully deletes the pod
and storage and zeroes the cost.

## Cost estimate per validation run

Typical breakdown on RTX A5000 at $0.27/hr:

- Pod boot + driver verification: 2 min ($0.009)
- Rust install + build: 8 min ($0.036)
- llama.cpp install + build + model download: 12 min ($0.054)
- Inferscope run + artefact capture: 5 min ($0.023)
- Buffer for unexpected debugging: 15 min ($0.068)

**Total estimate: ~45 minutes / $0.20 per run.**

A second run with a different GPU (A100 80GB at $1.39/hr for
the "money shot" article material) costs roughly $1.40 for
the same workflow — well within the $15 budget set up in the
RunPod onboarding session.

## Troubleshooting

**`nvidia-smi: command not found`** — pod template lacks the
driver. Destroy and re-deploy with a CUDA devel image.

**`cargo build` fails on `nvml-wrapper`** — verify pod has
`libssl-dev` and `pkg-config`: `apt-get install -y libssl-dev
pkg-config`.

**Process resource usage shows RSS ~2 MiB, 0% CPU, 1 thread** —
the PID supplied to `--pid` is a wrapper shell or transient
process, not the real `llama-server` worker. See the
"Finding the correct server PID" section under Step 6. Re-run
`pgrep -x llama-server` and pick a candidate whose
`/proc/<pid>/status` shows a sane VmRSS (hundreds of MiB or
more) and Threads > 1.

**`inferscope --gpu` says NVML unavailable inside the pod** —
the pod has CUDA but no NVML libraries. Try the
`runpod/pytorch` image rather than a bare `nvidia/cuda` base
(NVML is generally present in PyTorch images, sometimes absent
in minimal CUDA-only images).

**llama-server cannot find the model** — model download
incomplete (check size — Qwen 0.5B Q4 is ~400 MB).

**Pod becomes unresponsive** — refresh the RunPod console,
the pod may have hit an out-of-memory condition. Reboot from
the console, or terminate and start fresh.
