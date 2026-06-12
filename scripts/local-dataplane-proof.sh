#!/usr/bin/env bash
set -euo pipefail

# Local WireGuard/eBPF data-plane proof.
#
# Runs the gated `cargo test -p ployzd --test wireguard_dataplane` suite
# against real WireGuard interfaces and eBPF programs inside a privileged
# builder container.
#
# The linux artifacts it needs (ployz-ebpf-ctl, ployz-ebpf-tc bytecode) are
# built by scripts/build-dind-machine-image.sh — the only builder of linux
# artifacts/eBPF bytecode — and consumed from its output directory. They are
# built automatically when missing.
#
# The two-machine cluster proof that used to live here (Layer B) is owned by
# the Docker-in-Docker e2e harness: `scripts/dind-e2e.sh`,
# `crates/ployz-e2e/tests/dind_cluster.rs`, docs/operations/dind-e2e.md.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

IMAGE="${PLOYZ_LOCAL_DATAPLANE_IMAGE:-rust:1.91-bookworm}"
PROOF_IMAGE="${PLOYZ_LOCAL_DATAPLANE_PROOF_IMAGE:-ployz-local-dataplane-proof:rust-1.91-bookworm-v5}"
NATS_SERVER_VERSION="${PLOYZ_LOCAL_DATAPLANE_NATS_SERVER_VERSION:-2.14.2}"
TARGET_DIR="${PLOYZ_LOCAL_DATAPLANE_TARGET_DIR:-/tmp/ployz-local-dataplane-target}"
CARGO_REGISTRY_DIR="${PLOYZ_LOCAL_DATAPLANE_CARGO_REGISTRY_DIR:-/tmp/ployz-local-dataplane-cargo-registry}"
CARGO_GIT_DIR="${PLOYZ_LOCAL_DATAPLANE_CARGO_GIT_DIR:-/tmp/ployz-local-dataplane-cargo-git}"
DIND_TARGET_DIR="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
ARTIFACT_DIR="${DIND_TARGET_DIR}/release"
EBPF_CTL="${ARTIFACT_DIR}/ployz-ebpf-ctl"
EBPF_BYTECODE="${ARTIFACT_DIR}/ployz-ebpf-tc"
CONTAINER_NAME="ployz-local-dataplane-proof-$$"

command -v docker >/dev/null 2>&1 || {
  echo "docker is required for the local dataplane proof" >&2
  exit 1
}

cleanup() {
  docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

build_proof_image() {
  if docker image inspect "${PROOF_IMAGE}" >/dev/null 2>&1; then
    return 0
  fi

  docker build \
    --platform "$(docker_platform "${PLOYZ_LOCAL_DATAPLANE_PLATFORM:-}")" \
    --tag "${PROOF_IMAGE}" \
    - <<DOCKERFILE
FROM ${IMAGE}
ENV PATH="/usr/local/cargo/bin:\${PATH}"
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \\
  && apt-get install -y --no-install-recommends \\
    ca-certificates \\
    clang \\
    curl \\
    docker.io \\
    dbus \\
    iproute2 \\
    iptables \\
    lld \\
    llvm \\
    pkg-config \\
    systemd \\
    systemd-sysv \\
    wireguard-tools \\
  && rm -rf /var/lib/apt/lists/*
RUN arch="\$(dpkg --print-architecture)" \\
  && case "\${arch}" in amd64) nats_arch=amd64 ;; arm64) nats_arch=arm64 ;; *) echo "unsupported NATS arch: \${arch}" >&2; exit 1 ;; esac \\
  && curl -fsSL -o /tmp/nats-server.tar.gz "https://github.com/nats-io/nats-server/releases/download/v${NATS_SERVER_VERSION}/nats-server-v${NATS_SERVER_VERSION}-linux-\${nats_arch}.tar.gz" \\
  && mkdir -p /tmp/nats-server \\
  && tar -xzf /tmp/nats-server.tar.gz -C /tmp/nats-server --strip-components=1 \\
  && install -m 0755 /tmp/nats-server/nats-server /usr/local/bin/nats-server \\
  && rm -rf /tmp/nats-server /tmp/nats-server.tar.gz
RUN rustup install nightly \\
  && rustup component add rust-src --toolchain nightly \\
  && cargo +nightly install --locked bpf-linker
DOCKERFILE
}

ensure_dind_artifacts() {
  if [ -x "${EBPF_CTL}" ] && [ -f "${EBPF_BYTECODE}" ]; then
    return 0
  fi
  echo "linux artifacts missing; building via scripts/build-dind-machine-image.sh"
  PLOYZ_DIND_TARGET_DIR="${DIND_TARGET_DIR}" \
    bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh"
}

mkdir -p "${TARGET_DIR}" "${CARGO_REGISTRY_DIR}" "${CARGO_GIT_DIR}"
ensure_dind_artifacts
build_proof_image

docker run \
  --name "${CONTAINER_NAME}" \
  --detach \
  --platform "$(docker_platform "${PLOYZ_LOCAL_DATAPLANE_PLATFORM:-}")" \
  --privileged \
  --workdir /work \
  --volume "${ROOT_DIR}:/work" \
  --volume "${TARGET_DIR}:${TARGET_DIR}" \
  --volume "${ARTIFACT_DIR}:${ARTIFACT_DIR}:ro" \
  --volume "${CARGO_REGISTRY_DIR}:/usr/local/cargo/registry" \
  --volume "${CARGO_GIT_DIR}:/usr/local/cargo/git" \
  --tmpfs /run \
  --tmpfs /run/lock \
  "${PROOF_IMAGE}" \
  sleep infinity >/dev/null

docker exec "${CONTAINER_NAME}" bash -lc '
set -euo pipefail
export PATH="/usr/local/cargo/bin:${PATH}"
nohup dockerd --host=unix:///var/run/docker.sock --storage-driver=vfs >/tmp/ployz-dockerd.log 2>&1 &
for _ in $(seq 1 60); do
  if docker info >/dev/null 2>&1; then
    exit 0
  fi
  sleep 1
done
cat /tmp/ployz-dockerd.log >&2 || true
exit 1
'

docker exec "${CONTAINER_NAME}" bash -lc '
set -euo pipefail

export PATH="/usr/local/cargo/bin:${PATH}"
docker info >/dev/null

mountpoint -q /sys/fs/bpf || mount -t bpf bpf /sys/fs/bpf

export CARGO_TARGET_DIR='"${TARGET_DIR}"'

PLOYZ_LOCAL_DATAPLANE_PROOF=1 \
PLOYZ_LOCAL_DATAPLANE_EBPF_CTL='"${EBPF_CTL}"' \
PLOYZ_LOCAL_DATAPLANE_EBPF_BYTECODE='"${EBPF_BYTECODE}"' \
cargo test -p ployzd --test wireguard_dataplane -- --test-threads=1 --nocapture
'

echo "local WireGuard/eBPF dataplane proof passed"
