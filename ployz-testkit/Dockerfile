# syntax=docker/dockerfile:1
FROM rust:1.97.1-bookworm AS build
ARG TARGETARCH
WORKDIR /src
COPY . .
RUN --mount=type=cache,id=ployz-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ployz-target-${TARGETARCH},target=/src/target,sharing=locked \
    cargo build --locked --release --package ployzd \
 && cp /src/target/release/ployzd /ployzd

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl docker.io iproute2 iptables skopeo wireguard-tools \
 && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /opt/ployz/images \
 && skopeo copy docker://ghcr.io/unlabs-dev/corrosion:2026.6.15 docker-archive:/opt/ployz/images/corrosion.tar:ghcr.io/unlabs-dev/corrosion:2026.6.15
COPY --from=build /ployzd /usr/local/bin/ployzd
COPY ployz-testkit/entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint
RUN chmod +x /usr/local/bin/ployz-testkit-entrypoint \
 && dockerd --version \
 && wg --version \
 && ployzd version
VOLUME ["/var/lib/docker", "/var/lib/ployz"]
ENTRYPOINT ["ployz-testkit-entrypoint"]
