# Multi-stage build for inferscope
# Stage 1: build the Rust binary with gpu-nvidia feature
FROM rust:1.83-slim AS builder
WORKDIR /build
# Install build dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/*
# Copy source (assumes build context is inferscope repo root)
COPY . .
# Build release binary with GPU support
RUN cargo build --release --features gpu-nvidia
# Stage 2: minimal runtime with CUDA + NVML
FROM nvidia/cuda:13.0.2-runtime-ubuntu22.04
# OCI image labels for discoverability
LABEL org.opencontainers.image.title="inferscope"
LABEL org.opencontainers.image.description="Rust profiler for LLM inference engines with NVIDIA GPU sampling"
LABEL org.opencontainers.image.source="https://github.com/MicheleCampi/inferscope"
LABEL org.opencontainers.image.licenses="Apache-2.0"
LABEL org.opencontainers.image.authors="Michele Campi"
# Install minimal runtime deps (ca-certs for HTTPS calls to inference endpoint)
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates && \
    rm -rf /var/lib/apt/lists/*
# Copy binary from builder stage
COPY --from=builder /build/target/release/inferscope /usr/local/bin/inferscope
# Non-root user for security
RUN useradd -m -u 1000 -s /bin/bash inferscope
USER inferscope
WORKDIR /home/inferscope
ENTRYPOINT ["/usr/local/bin/inferscope"]
CMD ["--help"]
