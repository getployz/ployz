# syntax=docker/dockerfile:1
# Host binaries: cargo build --locked --release --package ployz --package ployzd
# then install them to ployz-testkit/prebuilt/{ployz,ployzd}.
# After docker build, prime the nested Docker graph into a tarball:
#   IMAGE=<tag> scripts/prime-testkit-image.sh
# Runtime glibc must match GitHub-hosted Ubuntu 24.04 (host cargo --release).
FROM docker:29.1.3-dind AS docker-bin

FROM ubuntu:24.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl dnsmasq dnsutils docker.io iproute2 iptables openssh-client openssh-server skopeo socat sqlite3 util-linux wireguard-tools \
 && rm -rf /var/lib/apt/lists/*
COPY --from=docker-bin /usr/local/bin/containerd /usr/local/bin/containerd-shim-runc-v2 /usr/local/bin/ctr /usr/local/bin/docker /usr/local/bin/docker-init /usr/local/bin/docker-proxy /usr/local/bin/dockerd /usr/local/bin/runc /usr/local/bin/
RUN mkdir -p /opt/ployz/images \
 && skopeo copy docker://ghcr.io/unlabs-dev/corrosion:2026.6.15 docker-archive:/opt/ployz/images/corrosion.tar:ghcr.io/unlabs-dev/corrosion:2026.6.15 \
 && skopeo copy docker://docker.io/library/alpine:3.23.3 docker-archive:/opt/ployz/images/alpine.tar:alpine:3.23.3 \
 && skopeo copy docker://docker.io/library/caddy:2.10.2 docker-archive:/opt/ployz/images/caddy.tar:caddy:2.10.2 \
 && skopeo copy docker://ghcr.io/zentinelproxy/zentinel@sha256:ff012547034d13a7d8e6570679c897e4bba6bc702ec5bdd7bf70a7a04b4d6604 docker-archive:/opt/ployz/images/zentinel.tar:ghcr.io/zentinelproxy/zentinel:26.08_5 \
 && skopeo copy docker://docker.io/envoyproxy/envoy@sha256:a707c3821b4cecb5db43d8e86e983e0f57b81010fefbabc01feeb071fb8cc08e docker-archive:/opt/ployz/images/envoy.tar:docker.io/envoyproxy/envoy:v1.35.13 \
 && skopeo copy docker://ghcr.io/psviderski/unregistry:0.4.1 docker-archive:/opt/ployz/images/unregistry.tar:ghcr.io/psviderski/unregistry:0.4.1
COPY ployz-testkit/prebuilt/ployz /usr/local/bin/ployz
COPY ployz-testkit/prebuilt/ployzd /usr/local/bin/ployzd
COPY ployz-testkit/entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint
COPY ployz-testkit/journalctl /usr/local/bin/journalctl
COPY ployz-testkit/daemon.json /etc/docker/daemon.json
COPY ployz-testkit/daemon-overlay.json /etc/docker/daemon-overlay.json
RUN chmod +x /usr/local/bin/ployz-testkit-entrypoint /usr/local/bin/journalctl \
 && dockerd --version \
 && wg --version \
 && ployzd version
VOLUME ["/var/lib/ployz"]
# Nested overlay cannot use a Docker graph committed into the image: snapshot
# dirs split across image/container layers and `docker create` fails with
# "overlay ... invalid argument". Keep the graph on one filesystem (this
# volume). Prime writes /opt/ployz/docker-graph.tar; entrypoint extracts it.
VOLUME ["/var/lib/docker"]
ENTRYPOINT ["ployz-testkit-entrypoint"]
