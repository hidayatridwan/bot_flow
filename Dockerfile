# syntax=docker/dockerfile:1

### Stage 1: build both workspace binaries
FROM rust:1.95-trixie AS builder
WORKDIR /build
COPY . .
# Cache the cargo registry + target dir across CI runs. Binaries are copied OUT of the
# cache mount (cache mounts don't persist into image layers).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release -p api -p worker \
    && cp target/release/api /build/api-bin \
    && cp target/release/worker /build/worker-bin

### Stage 2: API runtime (slim)
FROM debian:trixie-slim AS api
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libgomp1 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/api-bin /usr/local/bin/api
ENV RUST_LOG=info
EXPOSE 3000
CMD ["api"]

### Stage 3: Worker runtime (Rust binary + Python sidecar)
FROM debian:trixie-slim AS worker
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libgomp1 python3 python3-pip \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY sidecar/ /app/sidecar/
RUN pip3 install --break-system-packages --no-cache-dir -r /app/sidecar/requirements.txt
COPY --from=builder /build/worker-bin /usr/local/bin/worker
ENV RUST_LOG=info PARSER_PYTHON=python3 PARSER_SCRIPT=/app/sidecar/parser.py
CMD ["worker"]
