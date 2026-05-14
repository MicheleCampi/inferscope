# inferscope

> Profiling and observability for LLM inference engines.

[![CI](https://github.com/MicheleCampi/inferscope/actions/workflows/ci.yml/badge.svg)](https://github.com/MicheleCampi/inferscope/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust 1.83+](https://img.shields.io/badge/rust-1.83%2B-orange.svg)](rust-toolchain.toml)
[![Status: alpha](https://img.shields.io/badge/status-alpha-red.svg)](#status)

inferscope measures what an LLM inference engine actually does when
it serves a request. It drives an engine through its
OpenAI-compatible HTTP API, captures per-token timing, and
correlates that with the engine process resource footprint — so you
can see not just *how fast* a request was, but *where the time went*
and *whether resources were used well*.

It is engine-agnostic. Anything that speaks the OpenAI API —
llama.cpp's server, mistral.rs, vLLM, Ollama, TGI — is a target.

## Status

**Alpha. Under active development.** The public API and CLI surface
will change before v0.1.0 is tagged. This repository is being built
in the open; the commit history is the development log.

## What it measures

For each request sent to an engine, inferscope captures:

- **Time-to-first-token (TTFT)** — latency from request to the
  first generated token.
- **Inter-token latency** — the per-token generation cadence, and
  its distribution, from which real tokens-per-second and its
  variance are derived.
- **Total latency** — end-to-end request duration.
- **Process resource footprint** — resident memory, CPU
  utilization, and thread count of the engine process, sampled
  over the lifetime of the request.

The output is a structured report (text and JSON) that aggregates
these across a run.

## Scope

v0.1.0 is built on **API-level profiling**: the engine is treated
as a black box, observed through its HTTP API and the operating
system. This is engine-agnostic and ships as a complete, useful
tool.

**Engine-internal instrumentation** — attributing time and memory
to specific phases like KV cache management or attention
computation — is the direction for v0.2 and beyond. The reasoning
behind this sequencing is recorded in
[ADR-001](docs/adr/001-profiling-scope.md).

## Architecture

inferscope is a Cargo workspace of five crates, each with a single
responsibility:

| Crate | Responsibility |
|-------|----------------|
| `is-core` | Shared types: metrics, report structures, errors. Pure data, no I/O, no async. |
| `is-probe` | Drives an OpenAI-compatible endpoint and captures per-token timing. |
| `is-sysmon` | Samples the engine process resource footprint from `/proc`. |
| `is-report` | Renders collected metrics as structured text and JSON. |
| `inferscope` | The CLI binary that ties the layers together. |

Every crate depends on `is-core` for shared vocabulary. `is-core`
itself depends on nothing with a runtime, so the data definitions
stay free of I/O concerns.

## Building

inferscope pins a minimum supported Rust version of 1.83 via
`rust-toolchain.toml`. With a Rust toolchain installed:

    git clone https://github.com/MicheleCampi/inferscope.git
    cd inferscope
    cargo build --workspace

## Roadmap

- **v0.1.0** — API-level profiling: token timing, resource
  footprint correlation, text and JSON reports, CLI.
- **v0.2+** — engine-internal instrumentation: phase-level
  attribution of time and memory.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
