# syntax=docker/dockerfile:1
FROM rust:1.97.1-bookworm AS build
ARG TARGETARCH
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY ployz-core/Cargo.toml ployz-core/build.rs ployz-core/
COPY ployz-core/src ployz-core/src
COPY ployz/Cargo.toml ployz/
COPY ployz/src ployz/src
COPY ployzd/Cargo.toml ployzd/
COPY ployzd/src ployzd/src
COPY ployz-testkit/Cargo.toml ployz-testkit/
COPY ployz-testkit/src ployz-testkit/src
RUN --mount=type=cache,id=ployz-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ployz-target-${TARGETARCH},target=/src/target,sharing=locked \
    touch ployz-core/build.rs \
 && cargo build --locked --release --package ployzd \
 && cp /src/target/release/ployzd /ployzd

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl docker.io iproute2 iptables skopeo sqlite3 wireguard-tools \
 && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /opt/ployz/images \
 && skopeo copy docker://ghcr.io/unlabs-dev/corrosion:2026.6.15 docker-archive:/opt/ployz/images/corrosion.tar:ghcr.io/unlabs-dev/corrosion:2026.6.15 \
 && skopeo copy docker://docker.io/library/alpine:3.23.3 docker-archive:/opt/ployz/images/alpine.tar:alpine:3.23.3
COPY --from=build /ployzd /usr/local/bin/ployzd
COPY ployz-testkit/entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint
RUN chmod +x /usr/local/bin/ployz-testkit-entrypoint \
 && dockerd --version \
 && wg --version \
 && ployzd version
VOLUME ["/var/lib/docker", "/var/lib/ployz"]
ENTRYPOINT ["ployz-testkit-entrypoint"]
