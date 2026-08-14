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
COPY scripts/install.sh scripts/install.sh
RUN --mount=type=cache,id=ployz-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=ployz-target-${TARGETARCH},target=/src/target,sharing=locked \
    touch ployz-core/build.rs \
 && cargo build --locked --release --package ployz --package ployzd \
 && cp /src/target/release/ployz /ployz \
 && cp /src/target/release/ployzd /ployzd

FROM docker:29.1.3-dind AS docker-bin

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl dnsmasq dnsutils docker.io iproute2 iptables openssh-client openssh-server skopeo socat sqlite3 util-linux wireguard-tools \
 && rm -rf /var/lib/apt/lists/*
COPY --from=docker-bin /usr/local/bin/containerd /usr/local/bin/containerd-shim-runc-v2 /usr/local/bin/ctr /usr/local/bin/docker /usr/local/bin/docker-init /usr/local/bin/docker-proxy /usr/local/bin/dockerd /usr/local/bin/runc /usr/local/bin/
RUN mkdir -p /opt/ployz/images \
 && skopeo copy docker://ghcr.io/unlabs-dev/corrosion:2026.6.15 docker-archive:/opt/ployz/images/corrosion.tar:ghcr.io/unlabs-dev/corrosion:2026.6.15 \
 && skopeo copy docker://docker.io/library/alpine:3.23.3 docker-archive:/opt/ployz/images/alpine.tar:alpine:3.23.3 \
 && skopeo copy docker://docker.io/library/caddy:2.10.2 docker-archive:/opt/ployz/images/caddy.tar:caddy:2.10.2 \
 && skopeo copy docker://ghcr.io/psviderski/unregistry:0.4.1 docker-archive:/opt/ployz/images/unregistry.tar:ghcr.io/psviderski/unregistry:0.4.1
COPY --from=build /ployzd /usr/local/bin/ployzd
COPY --from=build /ployz /usr/local/bin/ployz
COPY ployz-testkit/entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint
COPY ployz-testkit/journalctl /usr/local/bin/journalctl
COPY ployz-testkit/daemon.json /etc/docker/daemon.json
COPY ployz-testkit/daemon-overlay.json /etc/docker/daemon-overlay.json
RUN chmod +x /usr/local/bin/ployz-testkit-entrypoint /usr/local/bin/journalctl \
 && dockerd --version \
 && wg --version \
 && ployzd version
VOLUME ["/var/lib/docker", "/var/lib/ployz"]
ENTRYPOINT ["ployz-testkit-entrypoint"]
