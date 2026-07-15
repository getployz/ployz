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
# In the default full mode it also saves workload image tarballs for the host
# architecture and builds the machine image. The artifacts-only mode stops
# after the release binaries and eBPF bytecode are ready.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

MACHINE_IMAGE="${PLOYZ_DIND_MACHINE_IMAGE:-ployz-dind-machine:local}"
BUILD_IMAGE="${PLOYZ_DIND_BUILD_IMAGE:-rust:1.91-bookworm}"
BUILDER_IMAGE="${PLOYZ_DIND_BUILDER_IMAGE:-ployz-dind-builder:rust-1.91-bookworm-v2}"
NATS_SERVER_VERSION="${PLOYZ_DIND_NATS_SERVER_VERSION:-2.14.2}"
WORKLOAD_IMAGE="${PLOYZ_DIND_WORKLOAD_IMAGE:-nginx:1.27-alpine}"
REGISTRY_IMAGE="${PLOYZ_DIND_REGISTRY_IMAGE:-registry:2.8.3}"
UMAMI_IMAGE="ghcr.io/umami-software/umami:postgresql-latest"
POSTGRES_IMAGE="postgres:15-alpine"
TARGET_DIR="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
EBPF_TARGET_DIR="${PLOYZ_DIND_EBPF_TARGET_DIR:-/tmp/ployz-dind-ebpf-target}"
CARGO_REGISTRY_DIR="${PLOYZ_DIND_CARGO_REGISTRY_DIR:-/tmp/ployz-dind-cargo-registry}"
CARGO_GIT_DIR="${PLOYZ_DIND_CARGO_GIT_DIR:-/tmp/ployz-dind-cargo-git}"
WORKLOAD_STAMP_DIR="${PLOYZ_DIND_WORKLOAD_STAMP_DIR:-${TARGET_DIR}/workload-image-stamps}"
CONTEXT_DIR="${PLOYZ_DIND_CONTEXT_DIR:-${ROOT_DIR}/docker/dind-machine}"
mode="${1:-full}"

case "${mode}" in
  full|artifacts-only|fingerprint) ;;
  *)
    echo "unknown build mode: ${mode} (supported: full, artifacts-only, fingerprint)" >&2
    exit 1
    ;;
esac

if [ "$#" -gt 1 ]; then
  echo "usage: $0 [full|artifacts-only|fingerprint]" >&2
  exit 1
fi

command -v docker >/dev/null 2>&1 || {
  echo "docker is required by this build script" >&2
  exit 1
}

image_digest() {
  docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$1"
}

machine_fingerprint() {
  local platform digest_dir workload_pid registry_pid umami_pid postgres_pid
  platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
  digest_dir="$(mktemp -d "${TMPDIR:-/tmp}/ployz-dind-fingerprint.XXXXXX")"
  image_digest "${WORKLOAD_IMAGE}" > "${digest_dir}/workload" & workload_pid=$!
  image_digest "${REGISTRY_IMAGE}" > "${digest_dir}/registry" & registry_pid=$!
  image_digest "${UMAMI_IMAGE}" > "${digest_dir}/umami" & umami_pid=$!
  image_digest "${POSTGRES_IMAGE}" > "${digest_dir}/postgres" & postgres_pid=$!
  wait "${workload_pid}"
  wait "${registry_pid}"
  wait "${umami_pid}"
  wait "${postgres_pid}"
  {
    printf '%s\0' \
      "platform=${platform}" \
      "nats=${NATS_SERVER_VERSION}" \
      "workload=${WORKLOAD_IMAGE}@$(<"${digest_dir}/workload")" \
      "registry=${REGISTRY_IMAGE}@$(<"${digest_dir}/registry")" \
      "umami=${UMAMI_IMAGE}@$(<"${digest_dir}/umami")" \
      "postgres=${POSTGRES_IMAGE}@$(<"${digest_dir}/postgres")"
    local file
    for file in Dockerfile daemon.json ployz-dind-images.service; do
      printf '%s\0' "${file}"
      cat "${CONTEXT_DIR}/${file}"
      printf '\0'
    done
    printf '%s\0' build-script
    cat "${BASH_SOURCE[0]}"
  } | sha256_stdin
  rm -rf "${digest_dir}"
}

if [ "${mode}" = "fingerprint" ]; then
  machine_fingerprint
  exit 0
fi

ensure_builder_image() {
  local want_platform existing_arch
  want_platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
  existing_arch="$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "${BUILDER_IMAGE}" 2>/dev/null || true)"
  if [ "${existing_arch}" = "${want_platform}" ]; then
    return 0
  fi

  docker build \
    --platform "${want_platform}" \
    --tag "${BUILDER_IMAGE}" \
    - <<DOCKERFILE
FROM ${BUILD_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \\
  && apt-get install -y --no-install-recommends clang cmake lld llvm pkg-config protobuf-compiler \\
  && rm -rf /var/lib/apt/lists/*
RUN rustup install nightly \\
  && rustup component add rust-src --toolchain nightly \\
  && cargo +nightly install --locked bpf-linker
DOCKERFILE
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

  ensure_builder_image
  mkdir -p "${TARGET_DIR}" "${CARGO_REGISTRY_DIR}" "${CARGO_GIT_DIR}"
  docker run --rm \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --env "CARGO_INCREMENTAL=${PLOYZ_DIND_CARGO_INCREMENTAL:-0}" \
    --volume "${ROOT_DIR}:/work" \
    --volume "${TARGET_DIR}:/target" \
    --volume "${CARGO_REGISTRY_DIR}:/usr/local/cargo/registry" \
    --volume "${CARGO_GIT_DIR}:/usr/local/cargo/git" \
    --workdir /work \
    "${BUILDER_IMAGE}" \
    bash -c 'set -euo pipefail
export PATH="/usr/local/cargo/bin:${PATH}"
cargo build --release --target-dir /target "$@"' \
    bash "${package_args[@]}"
}

build_ebpf_bytecode() {
  if [ "${PLOYZ_DIND_SKIP_BUILD:-0}" = "1" ]; then
    return 0
  fi

  ensure_builder_image
  mkdir -p "${EBPF_TARGET_DIR}" "${TARGET_DIR}/release"
  docker run --rm \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --env "CARGO_INCREMENTAL=${PLOYZ_DIND_CARGO_INCREMENTAL:-0}" \
    --volume "${ROOT_DIR}:/work" \
    --volume "${EBPF_TARGET_DIR}:/ebpf-target" \
    --volume "${TARGET_DIR}:/target" \
    --volume "${CARGO_REGISTRY_DIR}:/usr/local/cargo/registry" \
    --volume "${CARGO_GIT_DIR}:/usr/local/cargo/git" \
    --workdir /work \
    "${BUILDER_IMAGE}" \
    bash -lc 'set -euo pipefail
export PATH="/usr/local/cargo/bin:${PATH}"
bytecode="$(PLOYZ_EBPF_TARGET_DIR=/ebpf-target scripts/build-ebpf-bytecode.sh | tail -n 1)"
install -m 0644 "${bytecode}" /target/release/ployz-ebpf-tc'
}

bake_workload_tarball() {
  local platform name image save_image tar stamp image_id stamp_value temp_tar temp_stamp
  platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
  mkdir -p "${WORKLOAD_STAMP_DIR}"
  while read -r name image; do
    docker pull --platform "${platform}" "${image}"
    save_image="${image%@*}"
    if [ "${save_image}" != "${image}" ]; then
      docker tag "${image}" "${save_image}"
    fi
    tar="${CONTEXT_DIR}/${name}.tar"
    stamp="${WORKLOAD_STAMP_DIR}/${name}.stamp"
    image_id="$(docker image inspect --format '{{.Id}}' "${save_image}")"
    stamp_value="${platform} ${image} ${image_id}"
    if [ -f "${tar}" ] && [ "$(cat "${stamp}" 2>/dev/null || true)" = "${stamp_value}" ]; then
      continue
    fi

    temp_tar="${tar}.tmp.$$"
    temp_stamp="${stamp}.tmp.$$"
    if ! docker save -o "${temp_tar}" "${save_image}"; then
      rm -f "${temp_tar}" "${temp_stamp}"
      return 1
    fi
    mv "${temp_tar}" "${tar}"
    printf '%s\n' "${stamp_value}" > "${temp_stamp}"
    mv "${temp_stamp}" "${stamp}"
  done <<EOF
nginx ${WORKLOAD_IMAGE}
registry ${REGISTRY_IMAGE}
umami ${UMAMI_IMAGE}
postgres ${POSTGRES_IMAGE}
EOF
}

build_machine_image() {
  local fingerprint
  fingerprint="$(machine_fingerprint)"
  docker build \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --label dev.ployz.dind.managed=true \
    --label "dev.ployz.dind.fingerprint=${fingerprint}" \
    --build-arg "NATS_SERVER_VERSION=${NATS_SERVER_VERSION}" \
    --tag "${MACHINE_IMAGE}" \
    "${CONTEXT_DIR}"
}

build_linux_artifacts
build_ebpf_bytecode

if [ "${mode}" = "full" ]; then
  bake_workload_tarball
  build_machine_image
  echo "DinD machine image built: ${MACHINE_IMAGE}"
  echo "Host-arch linux artifacts (volume-mount these at test time):"
else
  echo "Linux release artifacts built:"
fi

cat <<EOF
  ployzd:         ${TARGET_DIR}/release/ployzd
  ployz:          ${TARGET_DIR}/release/ployz
  lease worker:   ${TARGET_DIR}/release/ployz-test-lease-worker
  ployz-ebpf-ctl: ${TARGET_DIR}/release/ployz-ebpf-ctl
  ployz-ebpf-tc:  ${TARGET_DIR}/release/ployz-ebpf-tc
EOF
