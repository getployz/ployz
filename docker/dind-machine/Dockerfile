# Machine image for the Docker-in-Docker e2e harness (plan C1).
#
# One container from this image is one ployz "machine": systemd as PID 1,
# an inner Docker daemon, and the runtime dependencies the keeper expects
# (WireGuard tools, iproute2, iptables). Debian bookworm keeps the glibc
# compatible with the rust:1.91-bookworm artifact builds.
#
# ployz binaries are deliberately NOT baked here — they are volume-mounted
# at test time so slow Rust rebuilds never go stale inside the image.
#
# Build context requirements (provided by scripts/build-dind-machine-image.sh):
#   nginx.tar — `docker save` of the workload image for the host arch.
FROM debian:bookworm

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    dbus \
    docker.io \
    iproute2 \
    iptables \
    systemd \
    systemd-sysv \
    wireguard-tools \
  && rm -rf /var/lib/apt/lists/*

# nats-server pinned to the version the repo's scripts pin (2.14.2), for the
# image's own architecture — never hardcode amd64.
ARG NATS_SERVER_VERSION=2.14.2
RUN arch="$(dpkg --print-architecture)" \
  && case "${arch}" in \
    amd64) nats_arch=amd64 ;; \
    arm64) nats_arch=arm64 ;; \
    *) echo "unsupported NATS arch: ${arch}" >&2; exit 1 ;; \
  esac \
  && curl -fsSL -o /tmp/nats-server.tar.gz "https://github.com/nats-io/nats-server/releases/download/v${NATS_SERVER_VERSION}/nats-server-v${NATS_SERVER_VERSION}-linux-${nats_arch}.tar.gz" \
  && mkdir -p /tmp/nats-server \
  && tar -xzf /tmp/nats-server.tar.gz -C /tmp/nats-server --strip-components=1 \
  && install -m 0755 /tmp/nats-server/nats-server /usr/local/bin/nats-server \
  && rm -rf /tmp/nats-server /tmp/nats-server.tar.gz

# Workload image tarball: a oneshot unit loads /images/*.tar into the inner
# Docker daemon after it starts, so inner daemons never pull from a registry.
COPY nginx.tar /images/nginx.tar
COPY ployz-dind-images.service /etc/systemd/system/ployz-dind-images.service
RUN systemctl enable docker.service ployz-dind-images.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
