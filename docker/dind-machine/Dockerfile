# Machine image for the Docker-in-Docker e2e harness (plan C1).
#
# One container from this image is one ployz "machine": systemd as PID 1,
# an inner Docker daemon, and the runtime dependencies the Host Runner expects
# (WireGuard tools, iproute2, iptables). Debian bookworm keeps the glibc
# compatible with the rust:1.91-bookworm artifact builds.
#
# ployz binaries are deliberately NOT baked here — they are volume-mounted
# at test time so slow Rust rebuilds never go stale inside the image.
#
# Build context requirements (provided by scripts/build-dind-machine-image.sh):
#   nginx.tar, registry.tar, umami.tar, and postgres.tar — `docker save`
#   archives for the host arch.
FROM debian:bookworm

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    dbus \
    gnupg \
    iproute2 \
    iptables \
    iputils-ping \
    procps \
    systemd \
    systemd-sysv \
    wireguard-tools \
  && install -m 0755 -d /etc/apt/keyrings \
  && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
  && chmod a+r /etc/apt/keyrings/docker.asc \
  && arch="$(dpkg --print-architecture)" \
  && echo "deb [arch=${arch} signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian bookworm stable" > /etc/apt/sources.list.d/docker.list \
  && apt-get update \
  && apt-get install -y --no-install-recommends \
    containerd.io=1.7.27-1 \
    docker-ce=5:28.5.2-1~debian.12~bookworm \
    docker-ce-cli=5:28.5.2-1~debian.12~bookworm \
  && rm -rf /var/lib/apt/lists/*

COPY daemon.json /etc/docker/daemon.json

# The harness allocates a TTY so journald can expose every machine's service
# output through `docker logs` while a scenario is running.
RUN install -d /etc/systemd/journald.conf.d \
  && printf '%s\n' \
    '[Journal]' \
    'ForwardToConsole=yes' \
    'TTYPath=/dev/console' \
    'MaxLevelConsole=debug' \
    > /etc/systemd/journald.conf.d/console.conf

# The inner containerd overlayfs snapshotter needs a non-overlay backing mount
# when the machine itself runs inside Docker.
VOLUME ["/var/lib/containerd"]

# nats-server pinned to the version the repo's scripts pin (2.14.2), for the
# image's own architecture — never hardcode amd64. The release tarball and its
# version stay baked at /opt/ployz/ so the e2e release manifest can name the
# archive as a local nats-server artifact source without touching the network.
ARG NATS_SERVER_VERSION=2.14.2
RUN arch="$(dpkg --print-architecture)" \
  && case "${arch}" in \
    amd64) nats_arch=amd64 ;; \
    arm64) nats_arch=arm64 ;; \
    *) echo "unsupported NATS arch: ${arch}" >&2; exit 1 ;; \
  esac \
  && mkdir -p /opt/ployz \
  && curl -fsSL -o /opt/ployz/nats-server.tar.gz "https://github.com/nats-io/nats-server/releases/download/v${NATS_SERVER_VERSION}/nats-server-v${NATS_SERVER_VERSION}-linux-${nats_arch}.tar.gz" \
  && printf '%s' "${NATS_SERVER_VERSION}" > /opt/ployz/nats-server.version \
  && mkdir -p /tmp/nats-server \
  && tar -xzf /opt/ployz/nats-server.tar.gz -C /tmp/nats-server --strip-components=1 \
  && install -m 0755 /tmp/nats-server/nats-server /usr/local/bin/nats-server \
  && rm -rf /tmp/nats-server

# Workload image tarball: a oneshot unit loads /images/*.tar into the inner
# Docker daemon after it starts, so inner daemons never pull from a registry.
COPY nginx.tar /images/nginx.tar
COPY registry.tar /images/registry.tar
COPY umami.tar /images/umami.tar
COPY postgres.tar /images/postgres.tar
COPY ployz-dind-images.service /etc/systemd/system/ployz-dind-images.service
RUN systemctl enable docker.service ployz-dind-images.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
