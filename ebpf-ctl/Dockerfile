FROM rust:1-bookworm AS builder

RUN rustup install nightly && \
    rustup component add rust-src --toolchain nightly && \
    cargo +nightly install bpf-linker

WORKDIR /build
COPY ebpf-common/ ebpf-common/
COPY ebpf/ ebpf/
COPY ebpf-ctl/ ebpf-ctl/

# aya-build uses `cargo build --package ployz-ebpf` which requires the
# package to be a workspace member. The ebpf crate targets bpfel-unknown-none
# but that's fine — aya-build shells out a separate cargo invocation with
# the correct target and -Z build-std=core.
RUN printf '[workspace]\nresolver = "2"\nmembers = ["ebpf-ctl", "ebpf", "ebpf-common"]\n' > Cargo.toml

WORKDIR /build/ebpf-ctl
RUN cargo +nightly build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    iproute2 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/ployz-ebpf-ctl /usr/local/bin/ployz-ebpf-ctl

CMD ["sleep", "infinity"]
