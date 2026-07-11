#!/usr/bin/env bash
set -euo pipefail

# Builds the Docker-in-Docker machine image for the ployz e2e harness.
#
# 1. Builds linux release binaries for the HOST docker architecture inside
#    rust:1.91-bookworm with cached target/registry dirs. The binaries are
#    NOT baked into the image — the harness volume-mounts them at test time
#    so the image is rebuilt rarely.
# 2. Builds the eBPF bytecode (bpfel target, host-arch-independent) inside a
#    derived builder image with nightly + bpf-linker baked, and copies it next
#    to the binaries so the Host Runner install spec can reference it.
# 3. Saves the workload image tarball for the host arch into the build
#    context so inner Docker daemons never pull from a registry.
# 4. Builds the machine image: systemd PID 1 + inner dockerd + nats-server.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

MACHINE_IMAGE="${PLOYZ_DIND_MACHINE_IMAGE:-ployz-dind-machine:local}"
BUILD_IMAGE="${PLOYZ_DIND_BUILD_IMAGE:-rust:1.91-bookworm}"
EBPF_BUILDER_IMAGE="${PLOYZ_DIND_EBPF_BUILDER_IMAGE:-ployz-dind-ebpf-builder:rust-1.91-bookworm-v1}"
NATS_SERVER_VERSION="${PLOYZ_DIND_NATS_SERVER_VERSION:-2.14.2}"
WORKLOAD_IMAGE="${PLOYZ_DIND_WORKLOAD_IMAGE:-nginx:1.27-alpine}"
REGISTRY_IMAGE="${PLOYZ_DIND_REGISTRY_IMAGE:-registry:2.8.3}"
UMAMI_IMAGE="${PLOYZ_DIND_UMAMI_IMAGE:-ghcr.io/umami-software/umami:postgresql-latest}"
POSTGRES_IMAGE="${PLOYZ_DIND_POSTGRES_IMAGE:-postgres:15-alpine}"
TARGET_DIR="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
EBPF_TARGET_DIR="${PLOYZ_DIND_EBPF_TARGET_DIR:-/tmp/ployz-dind-ebpf-target}"
CARGO_REGISTRY_DIR="${PLOYZ_DIND_CARGO_REGISTRY_DIR:-/tmp/ployz-dind-cargo-registry}"
CARGO_GIT_DIR="${PLOYZ_DIND_CARGO_GIT_DIR:-/tmp/ployz-dind-cargo-git}"
CONTEXT_DIR="${ROOT_DIR}/docker/dind-machine"

command -v docker >/dev/null 2>&1 || {
  echo "docker is required to build the DinD machine image" >&2
  exit 1
}

build_linux_artifacts() {
  if [ "${PLOYZ_DIND_SKIP_BUILD:-0}" = "1" ]; then
    return 0
  fi

  local crate
  local package_args=()
  for crate in "${PLOYZ_BINARY_CRATES[@]}"; do
    package_args+=("--package=${crate}")
  done

  mkdir -p "${TARGET_DIR}" "${CARGO_REGISTRY_DIR}" "${CARGO_GIT_DIR}"
  docker run --rm \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --volume "${ROOT_DIR}:/work" \
    --volume "${TARGET_DIR}:/target" \
    --volume "${CARGO_REGISTRY_DIR}:/usr/local/cargo/registry" \
    --volume "${CARGO_GIT_DIR}:/usr/local/cargo/git" \
    --workdir /work \
    "${BUILD_IMAGE}" \
    bash -c 'set -euo pipefail
export PATH="/usr/local/cargo/bin:${PATH}"
apt-get update
apt-get install -y --no-install-recommends cmake protobuf-compiler
rm -rf /var/lib/apt/lists/*
cargo build --release --target-dir /target "$@"' \
    bash "${package_args[@]}"
}

# The eBPF bytecode build needs nightly + rust-src + bpf-linker; baking them
# into a derived builder image keeps repeat builds fast.
build_ebpf_builder_image() {
  # An existing tag only counts when its architecture matches the target
  # platform: a stale image for another arch fails at run time.
  local want_platform existing_arch
  want_platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
  existing_arch="$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "${EBPF_BUILDER_IMAGE}" 2>/dev/null || true)"
  if [ "${existing_arch}" = "${want_platform}" ]; then
    return 0
  fi

  docker build \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --tag "${EBPF_BUILDER_IMAGE}" \
    - <<DOCKERFILE
FROM ${BUILD_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \\
  && apt-get install -y --no-install-recommends clang lld llvm pkg-config \\
  && rm -rf /var/lib/apt/lists/*
RUN rustup install nightly \\
  && rustup component add rust-src --toolchain nightly \\
  && cargo +nightly install --locked bpf-linker
DOCKERFILE
}

build_ebpf_bytecode() {
  if [ "${PLOYZ_DIND_SKIP_BUILD:-0}" = "1" ]; then
    return 0
  fi

  build_ebpf_builder_image
  mkdir -p "${EBPF_TARGET_DIR}" "${TARGET_DIR}/release"
  docker run --rm \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --volume "${ROOT_DIR}:/work" \
    --volume "${EBPF_TARGET_DIR}:/ebpf-target" \
    --volume "${TARGET_DIR}:/target" \
    --volume "${CARGO_REGISTRY_DIR}:/usr/local/cargo/registry" \
    --volume "${CARGO_GIT_DIR}:/usr/local/cargo/git" \
    --workdir /work \
    "${EBPF_BUILDER_IMAGE}" \
    bash -lc 'set -euo pipefail
export PATH="/usr/local/cargo/bin:${PATH}"
bytecode="$(PLOYZ_EBPF_TARGET_DIR=/ebpf-target scripts/build-ebpf-bytecode.sh | tail -n 1)"
install -m 0644 "${bytecode}" /target/release/ployz-ebpf-tc'
}

bake_workload_tarball() {
  docker pull --platform "$(docker_platform)" "${WORKLOAD_IMAGE}"
  docker pull --platform "$(docker_platform)" "${REGISTRY_IMAGE}"
  docker pull --platform "$(docker_platform)" "${UMAMI_IMAGE}"
  docker pull --platform "$(docker_platform)" "${POSTGRES_IMAGE}"
  rm -f "${CONTEXT_DIR}/nginx.tar"
  rm -f "${CONTEXT_DIR}/registry.tar"
  rm -f "${CONTEXT_DIR}/umami.tar"
  rm -f "${CONTEXT_DIR}/postgres.tar"
  docker save -o "${CONTEXT_DIR}/nginx.tar" "${WORKLOAD_IMAGE}"
  docker save -o "${CONTEXT_DIR}/registry.tar" "${REGISTRY_IMAGE}"
  docker save -o "${CONTEXT_DIR}/umami.tar" "${UMAMI_IMAGE}"
  docker save -o "${CONTEXT_DIR}/postgres.tar" "${POSTGRES_IMAGE}"
}

build_machine_image() {
  docker build \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --label dev.ployz.dind.managed=true \
    --build-arg "NATS_SERVER_VERSION=${NATS_SERVER_VERSION}" \
    --tag "${MACHINE_IMAGE}" \
    "${CONTEXT_DIR}"
}

build_linux_artifacts
build_ebpf_bytecode
bake_workload_tarball
build_machine_image

cat <<EOF
DinD machine image built: ${MACHINE_IMAGE}
Host-arch linux artifacts (volume-mount these at test time):
  ployzd:         ${TARGET_DIR}/release/ployzd
  ployz:          ${TARGET_DIR}/release/ployz
  lease worker:   ${TARGET_DIR}/release/ployz-lease-worker
  ployz-ebpf-ctl: ${TARGET_DIR}/release/ployz-ebpf-ctl
  ployz-ebpf-tc:  ${TARGET_DIR}/release/ployz-ebpf-tc
EOF
