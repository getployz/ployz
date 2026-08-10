# Machine image for the Docker-in-Docker e2e harness (plan C1).
#
# One container from this image is one ployz "machine": systemd as PID 1,
# an inner Docker daemon, and the runtime dependencies the Host Runner expects
# (WireGuard tools, iproute2, UFW/iptables, and eBPF inspection). Debian trixie supplies the minimum
# glibc required by the pinned Corrosion release while remaining compatible
# with binaries built by the older rust:1.91-bookworm builder.
#
# ployz binaries are deliberately NOT baked here — they are volume-mounted
# at test time so slow Rust rebuilds never go stale inside the image.
#
# Build context requirements (provided by scripts/build-dind-machine-image.sh):
#   nginx.tar and registry.tar — `docker save` archives for the host arch.
ARG BASE_IMAGE=debian:trixie
FROM ${BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    bpftool \
    curl \
    dbus \
    docker-cli=26.1.5+dfsg1-9+b13 \
    docker.io=26.1.5+dfsg1-9+b13 \
    git \
    iproute2 \
    iptables \
    iputils-ping \
    openssl \
    procps \
    python3 \
    systemd \
    systemd-sysv \
    ufw \
    wireguard-tools \
  && rm -rf /var/lib/apt/lists/*

COPY daemon.json /etc/docker/daemon.json
ARG DOCKER_HUB_MIRROR=mirror.gcr.io
ENV PLOYZ_BUILD_REGISTRY_MIRROR=${DOCKER_HUB_MIRROR}
RUN python3 -c 'import json, os; path="/etc/docker/daemon.json"; value=json.load(open(path)); value["registry-mirrors"]=["https://" + os.environ["PLOYZ_BUILD_REGISTRY_MIRROR"]]; open(path, "w").write(json.dumps(value, indent=2) + "\n")'
RUN install -d /etc/systemd/system.conf.d \
  && printf '%s\n' \
    '[Manager]' \
    "DefaultEnvironment=PLOYZ_BUILD_REGISTRY_MIRROR=${DOCKER_HUB_MIRROR}" \
    > /etc/systemd/system.conf.d/ployz-build-registry-mirror.conf

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

# Workload image tarball: a oneshot unit loads /images/*.tar into the inner
# Docker daemon after it starts, so inner daemons never pull from a registry.
COPY nginx.tar /images/nginx.tar
COPY registry.tar /images/registry.tar
COPY ployz-dind-images.service /etc/systemd/system/ployz-dind-images.service
RUN systemctl enable docker.service ployz-dind-images.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
