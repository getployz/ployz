FROM rust:1-bookworm AS builder

RUN rustup install nightly && \
    rustup target add --toolchain nightly x86_64-unknown-linux-musl aarch64-unknown-linux-musl && \
    rustup component add rust-src --toolchain nightly && \
    cargo +nightly install bpf-linker
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY ebpf-common/ ebpf-common/
COPY ebpf/ ebpf/
COPY ebpf-ctl/ ebpf-ctl/

RUN printf '[workspace]\nresolver = "2"\nmembers = ["ebpf-ctl", "ebpf", "ebpf-common"]\n' > Cargo.toml

WORKDIR /build/ebpf-ctl
ARG TARGETARCH
RUN if [ "$TARGETARCH" = "arm64" ]; then TARGET=aarch64-unknown-linux-musl; else TARGET=x86_64-unknown-linux-musl; fi && \
    cargo +nightly build --release --target "$TARGET" && \
    cp /build/target/"$TARGET"/release/ployz-ebpf-ctl /build/target/release/ployz-ebpf-ctl

FROM alpine:3.21
RUN apk add --no-cache wireguard-tools iproute2
COPY --from=builder /build/target/release/ployz-ebpf-ctl /usr/local/bin/ployz-ebpf-ctl
CMD ["sleep", "infinity"]