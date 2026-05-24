# Security Policy

This document describes the security model of `inferscope`: what it does, where the trust boundaries are, and how to report vulnerabilities. It is intentionally specific rather than boilerplate — generic security policies are not credible, and `inferscope` has a small but non-trivial attack surface that deserves a real explanation.

## Reporting a Vulnerability

If you find a security issue, please use one of these two channels:

1. **GitHub Security Advisories** (preferred): open a private advisory at
   https://github.com/MicheleCampi/inferscope/security/advisories/new
2. **Email**: `michele.campi@outlook.com` with subject prefix `[INFERSCOPE-SEC]`

**Do not open a public issue** for security matters.

**Response expectations:**
- Acknowledgement within 72 hours
- Initial triage within 7 days
- Fix or mitigation timeline communicated within 14 days

This is a single-maintainer project, so timelines reflect that. For coordinated disclosure, please allow at least 30 days between report and public discussion.

## What inferscope Is (and Isn't)

`inferscope` is a profiling CLI that drives an OpenAI-compatible LLM inference engine through its HTTP API, while in parallel sampling the engine process's resource usage from `/proc` and (optionally) the NVIDIA GPU state via NVML. It outputs a plain-text report and a JSON document. It runs to completion and exits.

It is **not**:
- A daemon, a service, or a network listener — it makes outbound HTTP and reads local files, nothing else.
- A privileged tool — it runs as an unprivileged user and uses no `setuid`, no `CAP_SYS_ADMIN`, no kernel modules.
- An auth boundary — it doesn't authenticate users or enforce access control. The user who runs it has whatever access their shell has.

This shapes the threat model: most classical service vulnerabilities (auth bypass, injection in stored payloads, CSRF, session hijacking) do not apply.

## Threat Model

| Actor                              | Primary concern                                                                                |
|------------------------------------|------------------------------------------------------------------------------------------------|
| Malicious inference endpoint       | Returns crafted SSE/JSON payload to exploit `inferscope`'s HTTP parsing or trigger DoS         |
| Compromised local user account     | Reads `/proc/<pid>` of processes owned by other users on the same host                         |
| Supply chain attacker              | Compromised `nvml-wrapper`, `reqwest`, `tokio`, or other dependency injects malicious code     |
| Side-channel observer (NVML)       | Uses `inferscope`'s sampling cadence as a covert channel between processes on shared hardware  |
| Container escape via image         | Distributes a malicious build of the `inferscope` Docker image impersonating the official one  |

The threat model **does not include**:
- Nation-state adversaries with control over NVIDIA drivers or the Linux kernel
- Compromise of upstream Rust toolchain, `crates.io`, or rustup
- Physical access to the maintainer's workstation or to the host running `inferscope`

## Architecture and Trust Boundaries

`inferscope` has three points where untrusted data enters the process:

```
┌──────────────────────────────────────────────────────────────────┐
│  Operator                                                        │
│  (CLI args: --endpoint, --model, --prompt, --pid, --gpu)         │
└────────────────┬─────────────────────────────────────────────────┘
                 │ trusted input (the operator chose to run this)
                 ▼
┌──────────────────────────────────────────────────────────────────┐
│  inferscope process                                              │
│                                                                  │
│  ┌──────────────┐   ┌──────────────┐   ┌──────────────────────┐  │
│  │  is-probe    │   │  is-sysmon   │   │  is-sysmon (gpu)     │  │
│  │  HTTP/SSE    │   │  /proc reads │   │  NVML read-only      │  │
│  └──────┬───────┘   └──────┬───────┘   └──────────┬───────────┘  │
└─────────┼──────────────────┼──────────────────────┼──────────────┘
          │                  │                      │
          ▼                  ▼                      ▼
   ┌──────────────┐   ┌──────────────┐    ┌──────────────────────┐
   │  Inference   │   │  Kernel      │    │  NVIDIA driver       │
   │  endpoint    │   │  (procfs)    │    │  (libnvidia-ml.so)   │
   │  UNTRUSTED   │   │  trusted     │    │  trusted             │
   └──────────────┘   └──────────────┘    └──────────────────────┘
```

The inference endpoint is the only **untrusted** input source by design. `/proc` reads and NVML calls go through trusted kernel/driver interfaces; the operator chose the `--pid` and `--gpu` flags, so the choice itself is in-scope of operator intent.

## Controls

### HTTP probe layer (`is-probe`)

The probe issues a single POST to the configured endpoint and reads a streamed Server-Sent Events response. Defensive properties:

- **Request body is constructed by `inferscope`** from CLI args; the endpoint cannot influence what is sent.
- **Response is parsed via `serde_json`**, which is memory-safe and bounded by the streaming reader's buffer.
- **`reqwest` with `tokio`** handles the HTTP layer; both have a strong CVE history and active maintenance.
- **No connection reuse across invocations**: the process exits after one run, so no long-lived connection state persists.
- **No retries on the probe path**: a malicious endpoint cannot cause the process to issue repeated requests.
- **No follow of redirects to other hosts**: `reqwest` defaults are kept (max 10 redirects on the same scheme/host policy).
- **Timeouts**: there is a per-request timeout in `is-probe`. A misbehaving endpoint cannot stall `inferscope` indefinitely.

### Process sampling layer (`is-sysmon`)

The sysmon reads `/proc/<pid>/stat`, `/proc/<pid>/status`, and `/proc/<pid>/task/<pid>/children`. With `--include-descendants`, it also reads `/proc/<child_pid>/...` for direct children.

- **No write to `/proc`**: every operation is a read.
- **Scope is bound to the PID supplied via `--pid`** and (optionally) its direct children. There is no recursive walk of the process tree, no scan of other PIDs.
- **`/proc` permissions are enforced by the kernel**: reading `/proc/<pid>` of another user's process fails unless `inferscope` runs as root or `hidepid=0` is configured. `inferscope` does not attempt to bypass this.
- **Parsers are pure functions** (`is_sysmon::parse::*`) with unit tests covering malformed input. Crash on bad input is preferred to silent misreporting.

### GPU sampling layer (`gpu-nvidia` feature)

NVML is accessed via `nvml-wrapper` v0.12.1. All calls are read-only metric queries (utilization, memory, power, temperature). `inferscope` never invokes NVML functions that change GPU state.

- **No `nvmlDeviceSetPowerManagementLimit` or similar mutating calls**.
- **NVML errors degrade gracefully**: a failed sample is recorded as absent in the timeline rather than crashing the run.
- **The `gpu-nvidia` feature is opt-in at build time**. A binary built without it cannot touch NVML at all.

### Dependency hygiene

- **MSRV pinned to Rust 1.83** via `rust-toolchain.toml`. This guarantees reproducible builds across contributor machines and CI.
- **`Cargo.lock` is committed**. Direct and transitive dependencies are version-locked.
- **CI runs on every push** against the locked dependency tree, exercising all 120+ tests under `-D warnings`. Dependency drift surfaces as compilation failure, not silent regression.
- **`cargo audit` is not yet wired into CI.** This is a known gap; the issue is tracked, and the planned remediation is a `cargo deny check advisories` step in `.github/workflows/ci.yml`.
- **No `unsafe` code** in the workspace's own crates (`is-core`, `is-probe`, `is-sysmon`, `is-report`, `inferscope`). All `unsafe` usage is contained inside transitive dependencies, which are themselves audited by the broader Rust ecosystem.

### Docker image (added 2026-05-24)

The `Dockerfile` at the repository root produces a non-root image. Defensive properties:

- **Multi-stage build**: the final image contains only the release binary and minimal runtime deps (`ca-certificates`), not the Rust toolchain.
- **Non-root user `inferscope` (UID 1000)** is the default `USER`.
- **No `SUID`/`SGID`** binaries added.
- **Base image is `nvidia/cuda:13.0.2-runtime-ubuntu22.04`**, an official NVIDIA image. Upstream image rebuilds are tracked by Docker Hub; consumers are expected to pull on a schedule.
- **No secrets baked into the image**: there are no API keys, tokens, or credentials at any layer.

### Data handling

`inferscope` processes data the operator already has access to:
- The prompt, supplied as a CLI argument.
- The inference response, returned by the endpoint the operator chose to query.
- The `/proc` data of a PID the operator named.
- NVML data for GPUs already accessible to the operator's process.

The JSON report is written to stdout (or redirected by the operator). Nothing is persisted beyond what the operator's shell does with the output.

There is **no telemetry, no phone-home, no analytics call** of any kind. The binary makes exactly one HTTP destination: the `--endpoint` URL the operator passes in.

## Known Limitations

Honest enumeration of where the security posture has room to grow:

1. **No `cargo audit` or `cargo deny` in CI yet.** Until this lands, dependency advisories are tracked manually via GitHub Dependabot alerts.
2. **No automated SAST.** Rust's borrow checker plus `-D warnings` clippy gating is the entire static analysis story.
3. **Docker image is not signed.** No `cosign` signature is published. A consumer cannot cryptographically verify they pulled the image built from a specific commit.
4. **Single maintainer = single point of failure for response.** If the maintainer is unreachable, security reports may queue. There is no on-call rotation.
5. **No formal SLA for fix delivery.** Response times above are best-effort, not contractual.
6. **No penetration test has been performed.** The project is an open-source tool, not a regulated product.

## Out of Scope

The following are intentionally not handled by this policy:

- Vulnerabilities in inference engines (`llama.cpp`, `vLLM`, `TGI`, etc.) — report to those projects.
- Vulnerabilities in NVIDIA drivers or NVML — report to NVIDIA.
- Vulnerabilities in Rust dependencies — report upstream and to GitHub Dependabot.
- Bugs that do not have a security impact — open a regular GitHub issue.
- Profiling-result correctness — these are measurement bugs, not security issues.

## Changelog

- **2026-05-24**: Initial security policy published. Reflects current state: v0.2.1 with `gpu-nvidia` feature, process-tree aggregation via `--include-descendants`, `Dockerfile` published the same day, CI green on `-D warnings` with `fmt` + `clippy` + `test` jobs.
