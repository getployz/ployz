# syntax=docker/dockerfile:1
# The testkit base: OS, Docker and the nested images, with no ployz binaries.
# scripts/testkit-image.sh builds it, primes it with
# scripts/prime-testkit-image.sh, and layers this checkout's ployz, ployzd and
# entrypoint on top. Those host binaries need Ubuntu 24.04's glibc.
FROM docker:29.1.3-dind AS docker-bin

FROM ubuntu:24.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates curl dnsmasq dnsutils docker.io git iproute2 iptables openssh-client openssh-server skopeo socat sqlite3 util-linux wireguard-tools \
 && rm -rf /var/lib/apt/lists/*
COPY --from=docker-bin /usr/local/bin/containerd /usr/local/bin/containerd-shim-runc-v2 /usr/local/bin/ctr /usr/local/bin/docker /usr/local/bin/docker-init /usr/local/bin/docker-proxy /usr/local/bin/dockerd /usr/local/bin/runc /usr/local/bin/
COPY --from=docker-bin /usr/local/bin/dind /usr/local/bin/dind
COPY --from=docker-bin /usr/local/libexec/docker/cli-plugins/docker-buildx /usr/local/libexec/docker/cli-plugins/docker-buildx
# Corrosion goes in as an OCI archive, which keeps its index digest, so the
# containerd store finds the digest-pinned IMAGE in ployzd's corrosion/service.rs.
# The classic store cannot load an OCI archive; only priming, which runs on the
# containerd store, loads these tars.
RUN mkdir -p /opt/ployz/images \
 && skopeo copy --all docker://ghcr.io/getployz/corrosion@sha256:384f1e20dbbe187135c4c75ae0eb41943df64c2306b5b4eecffa88ba04b46cc1 oci-archive:/opt/ployz/images/corrosion.tar:ghcr.io/getployz/corrosion:20261031-28f00b417271 \
 && skopeo copy docker://docker.io/library/alpine:3.23.3 docker-archive:/opt/ployz/images/alpine.tar:alpine:3.23.3 \
 && skopeo copy docker://docker.io/library/caddy:2.10.2 docker-archive:/opt/ployz/images/caddy.tar:caddy:2.10.2 \
 && skopeo copy docker://ghcr.io/psviderski/unregistry:0.4.1 docker-archive:/opt/ployz/images/unregistry.tar:ghcr.io/psviderski/unregistry:0.4.1
COPY crates/ployz-testkit/entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint
COPY crates/ployz-testkit/journalctl /usr/local/bin/journalctl
COPY crates/ployz-testkit/daemon.json /etc/docker/daemon.json
COPY crates/ployz-testkit/daemon-overlay.json /etc/docker/daemon-overlay.json
RUN chmod +x /usr/local/bin/ployz-testkit-entrypoint /usr/local/bin/journalctl \
 && dockerd --version \
 && wg --version
VOLUME ["/var/lib/ployz"]
# Nested overlay cannot use a Docker graph committed into the image: snapshot
# dirs split across image/container layers and `docker create` fails with
# "overlay ... invalid argument". Keep the graph on one filesystem (this
# volume). Prime writes /opt/ployz/docker-graph.tar; entrypoint extracts it.
VOLUME ["/var/lib/docker"]
# Upstream DinD enables nested cgroup controllers before resource-limited workloads.
ENTRYPOINT ["dind", "ployz-testkit-entrypoint"]
