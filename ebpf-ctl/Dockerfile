FROM rust:1-bookworm AS builder

RUN rustup install nightly && \
    rustup component add rust-src --toolchain nightly && \
    cargo +nightly install bpf-linker

WORKDIR /build
COPY ebpf-common/ ebpf-common/
COPY ebpf/ ebpf/
COPY ebpf-ctl/ ebpf-ctl/

WORKDIR /build/ebpf-ctl
RUN cargo +nightly build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    iproute2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/ebpf-ctl/target/release/ployz-ebpf-ctl /usr/local/bin/ployz-ebpf-ctl

ENTRYPOINT ["sleep", "infinity"]
