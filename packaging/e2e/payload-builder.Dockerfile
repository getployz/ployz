FROM rust:1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    cmake \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*
