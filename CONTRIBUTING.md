# Contributing to inferscope

## Pre-flight before pushing

CI runs five jobs on every push to `main` and every pull request. Two
gates reproduce them locally. Run both before pushing; a red run stays
in the repository's history permanently.

The canonical gate covers the default build — what `cargo install
inferscope` produces and what the Docker image ships:

    cargo fmt --all --check
    RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets
    cargo test --workspace

The all-features gate covers the optional features, `gpu-nvidia` and
`otel-export`:

    RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
    cargo test --workspace --all-features

Both are required. They are not redundant: the first is the only one
that proves the default build is clean, and the second is the only one
that compiles the code behind a feature gate at all.

### Why the second gate exists

The `otel-export` feature (ADR-008) did not compile from 2026-07-18 to
2026-07-29, and shipped broken in the v0.4.0 tag cut on 2026-07-25.
Three compile errors sat in
`crates/is-report/src/otel.rs` while every CI job passed, because no job
built with `--all-features` and the module's own unit test was never
reached. The README and RUNBOOK told users to build with that feature;
the command failed for anyone who tried.

A gate that does not look at a configuration does not protect it. Both
optional features are behind `#[cfg(feature = ...)]`, so code inside
them is not type-checked at all unless something asks for it.

## Test counts

Test totals depend on which gate produced them, and any figure quoted
elsewhere should say which:

    cargo test --workspace                  # canonical
    cargo test --workspace --all-features   # includes gpu-nvidia, otel-export

`gpu-nvidia` tests need neither a GPU nor a driver: `nvml-wrapper`
resolves `libnvidia-ml.so` at runtime, and the tests that would need it
assert the unavailable path. They run on any CI runner.
