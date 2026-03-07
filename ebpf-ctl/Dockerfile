FROM rust:1-bookworm AS builder

RUN rustup install nightly && \
    rustup component add rust-src --toolchain nightly && \
    cargo +nightly install bpf-linker

WORKDIR /build
COPY ebpf-common/ ebpf-common/
COPY ebpf/ ebpf/
COPY ebpf-ctl/ ebpf-ctl/

RUN printf '[workspace]\nresolver = "2"\nmembers = ["ebpf-ctl", "ebpf", "ebpf-common"]\n' > Cargo.toml

WORKDIR /build/ebpf-ctl
RUN cargo +nightly build --release

FROM alpine:3.21
RUN apk add --no-cache wireguard-tools iproute2
COPY --from=builder /build/target/release/ployz-ebpf-ctl /usr/local/bin/ployz-ebpf-ctl
CMD ["sleep", "infinity"]