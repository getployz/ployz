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
MACHINE_BASE_IMAGE="debian:trixie"
BUILDER_IMAGE="${PLOYZ_DIND_BUILDER_IMAGE:-ployz-dind-builder:rust-1.91-bookworm-v2}"
DOCKER_HUB_MIRROR="${PLOYZ_DIND_DOCKER_HUB_MIRROR:-mirror.gcr.io}"
WORKLOAD_IMAGE="${PLOYZ_DIND_WORKLOAD_IMAGE:-nginx:1.27-alpine}"
REGISTRY_IMAGE="${PLOYZ_DIND_REGISTRY_IMAGE:-registry:2.8.3}"
UMAMI_IMAGE="${PLOYZ_DIND_UMAMI_IMAGE:-ghcr.io/umami-software/umami:postgresql-latest@sha256:8edfe4beaef13f9d1300619fa264ef250a3688df9cc54d24ca830ca31cb475ec}"
POSTGRES_IMAGE="${PLOYZ_DIND_POSTGRES_IMAGE:-postgres:15-alpine@sha256:3d0f7584ed7d04e27fa050d6683a74746608faf21f202be78460d679cc56461f}"
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

validate_registry_mirror() {
  local value="$1" host port label
  case "${value}" in
    ""|*[^a-z0-9.:-]*|*::*|.*|*.|*:)
      echo "PLOYZ_DIND_DOCKER_HUB_MIRROR must be a lowercase DNS host with an optional port" >&2
      return 1
      ;;
  esac
  host="${value%%:*}"
  port="${value#*:}"
  if [ "${#host}" -gt 253 ]; then
    echo "PLOYZ_DIND_DOCKER_HUB_MIRROR host must not exceed 253 characters" >&2
    return 1
  fi
  if [ "${port}" != "${value}" ]; then
    case "${port}" in
      ""|0|0*|*[!0-9]*)
        echo "PLOYZ_DIND_DOCKER_HUB_MIRROR has an invalid port" >&2
        return 1
        ;;
    esac
    if [ "${#port}" -gt 5 ] || [ "${port}" -gt 65535 ]; then
      echo "PLOYZ_DIND_DOCKER_HUB_MIRROR port must be between 1 and 65535" >&2
      return 1
    fi
  fi
  IFS=. read -r -a labels <<< "${host}"
  for label in "${labels[@]}"; do
    if [ -z "${label}" ] || [ "${#label}" -gt 63 ] \
      || [[ ! "${label}" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]]; then
      echo "PLOYZ_DIND_DOCKER_HUB_MIRROR contains an invalid DNS label" >&2
      return 1
    fi
  done
}

docker_hub_source() {
  local image="$1" first remainder
  first="${image%%/*}"
  if [ "${first}" = "${image}" ]; then
    printf '%s/library/%s\n' "${DOCKER_HUB_MIRROR}" "${image}"
  elif [ "${first}" = "docker.io" ] \
    || [ "${first}" = "index.docker.io" ] \
    || [ "${first}" = "registry-1.docker.io" ]; then
    remainder="${image#*/}"
    if [[ "${remainder}" == */* ]]; then
      printf '%s/%s\n' "${DOCKER_HUB_MIRROR}" "${remainder}"
    else
      printf '%s/library/%s\n' "${DOCKER_HUB_MIRROR}" "${remainder}"
    fi
  elif [[ "${first}" == *.* ]] || [[ "${first}" == *:* ]] || [ "${first}" = "localhost" ]; then
    printf '%s\n' "${image}"
  else
    printf '%s/%s\n' "${DOCKER_HUB_MIRROR}" "${image}"
  fi
}

validate_registry_mirror "${DOCKER_HUB_MIRROR}"
BUILD_IMAGE_SOURCE="$(docker_hub_source "${BUILD_IMAGE}")"
MACHINE_BASE_IMAGE_SOURCE="$(docker_hub_source "${MACHINE_BASE_IMAGE}")"
WORKLOAD_IMAGE_SOURCE="$(docker_hub_source "${WORKLOAD_IMAGE}")"
REGISTRY_IMAGE_SOURCE="$(docker_hub_source "${REGISTRY_IMAGE}")"
UMAMI_IMAGE_SOURCE="$(docker_hub_source "${UMAMI_IMAGE}")"
POSTGRES_IMAGE_SOURCE="$(docker_hub_source "${POSTGRES_IMAGE}")"
WORKLOAD_NAMES=(nginx registry umami postgres)
WORKLOAD_IMAGES=("${WORKLOAD_IMAGE}" "${REGISTRY_IMAGE}" "${UMAMI_IMAGE}" "${POSTGRES_IMAGE}")
WORKLOAD_SOURCES=("${WORKLOAD_IMAGE_SOURCE}" "${REGISTRY_IMAGE_SOURCE}" "${UMAMI_IMAGE_SOURCE}" "${POSTGRES_IMAGE_SOURCE}")

image_digest() {
  docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$1"
}

machine_fingerprint() (
  local platform digest_dir failed name pid file index
  local -a digest_pids=()
  platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
  digest_dir="$(mktemp -d "${TMPDIR:-/tmp}/ployz-dind-fingerprint.XXXXXX")"
  trap 'rm -rf "${digest_dir}"' EXIT
  for index in "${!WORKLOAD_NAMES[@]}"; do
    image_digest "${WORKLOAD_SOURCES[index]}" > "${digest_dir}/${WORKLOAD_NAMES[index]}" &
    digest_pids+=("$!")
  done
  failed=0
  for pid in "${digest_pids[@]}"; do
    if ! wait "${pid}"; then
      failed=1
    fi
  done
  if [ "${failed}" -ne 0 ]; then
    echo "one or more DinD manifest inspections failed" >&2
    return 1
  fi
  for name in "${WORKLOAD_NAMES[@]}"; do
    if [ ! -s "${digest_dir}/${name}" ]; then
      echo "DinD manifest inspection returned an empty digest for ${name}" >&2
      return 1
    fi
  done
  {
    printf '%s\0' \
      "platform=${platform}" \
      "docker_hub_mirror=${DOCKER_HUB_MIRROR}" \
      "machine_base_image=${MACHINE_BASE_IMAGE_SOURCE}"
    for index in "${!WORKLOAD_NAMES[@]}"; do
      name="${WORKLOAD_NAMES[index]}"
      printf '%s\0' "image.${name}=${WORKLOAD_IMAGES[index]}|${WORKLOAD_SOURCES[index]}@$(<"${digest_dir}/${name}")"
    done
    for file in Dockerfile daemon.json ployz-dind-images.service; do
      printf '%s\0' "${file}"
      cat "${CONTEXT_DIR}/${file}"
      printf '\0'
    done
    printf '%s\0' build-script
    cat "${BASH_SOURCE[0]}"
  } | sha256_stdin
)

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
FROM ${BUILD_IMAGE_SOURCE}
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

stage_corrosion() {
  local platform corrosion_platform manifest_file pin_file release_tag archive_name archive_url archive_sha256
  local embedded_version archive cache_dir actual_sha256 work_dir host_uid host_gid
  local -a corrosion_pin
  platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
  case "${platform}" in
    linux/amd64) corrosion_platform="linux-amd64" ;;
    linux/arm64) corrosion_platform="linux-arm64" ;;
    *)
      echo "the pinned Corrosion DinD assets support linux/amd64 and linux/arm64, got ${platform}" >&2
      exit 1
      ;;
  esac
  ensure_builder_image
  docker pull --platform "${platform}" "${MACHINE_BASE_IMAGE_SOURCE}"

  command -v python3 >/dev/null 2>&1 || {
    echo "python3 is required to read corrosion-release.json" >&2
    exit 1
  }
  manifest_file="${ROOT_DIR}/corrosion-release.json"
  pin_file="$(mktemp "${TMPDIR:-/tmp}/ployz-corrosion-pin.XXXXXX")"
  if ! python3 "${ROOT_DIR}/scripts/read-corrosion-release.py" \
    "${manifest_file}" "${corrosion_platform}" > "${pin_file}"
  then
    rm -f "${pin_file}"
    return 1
  fi
  mapfile -t corrosion_pin < "${pin_file}"
  rm -f "${pin_file}"
  if [ "${#corrosion_pin[@]}" -ne 5 ]; then
    echo "corrosion release manifest did not yield exactly five pinned fields" >&2
    exit 1
  fi
  release_tag="${corrosion_pin[0]}"
  archive_name="${corrosion_pin[1]}"
  archive_url="${corrosion_pin[2]}"
  archive_sha256="${corrosion_pin[3]}"
  embedded_version="${corrosion_pin[4]}"

  if [ -n "${PLOYZ_CORROSION_ARCHIVE:-}" ]; then
    archive="${PLOYZ_CORROSION_ARCHIVE}"
    if [ ! -f "${archive}" ]; then
      echo "PLOYZ_CORROSION_ARCHIVE does not name a file: ${archive}" >&2
      exit 1
    fi
  else
    cache_dir="${ROOT_DIR}/target/corrosion-cache/${release_tag}"
    archive="${cache_dir}/${archive_name}"
    mkdir -p "${cache_dir}"
    if [ ! -f "${archive}" ]; then
      curl --fail --location --retry 3 --silent --show-error \
        "${archive_url}" --output "${archive}.partial"
      mv "${archive}.partial" "${archive}"
    fi
  fi

  actual_sha256="$(sha256_of "${archive}")"
  if [ "${actual_sha256}" != "${archive_sha256}" ]; then
    echo "Corrosion release archive has SHA-256 ${actual_sha256}, expected ${archive_sha256}" >&2
    exit 1
  fi

  work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ployz-dind-corrosion.XXXXXX")"
  tar -xzf "${archive}" -C "${work_dir}" corrosion
  if ! verify_corrosion_embedded_version \
    "${platform}" "${work_dir}" "${MACHINE_BASE_IMAGE_SOURCE}" "${embedded_version}"; then
    rm -rf "${work_dir}"
    return 1
  fi
  mkdir -p "${TARGET_DIR}/release"
  host_uid="$(id -u)"
  host_gid="$(id -g)"
  docker run --rm \
    --platform "${platform}" \
    --volume "${TARGET_DIR}:/target" \
    "${BUILDER_IMAGE}" \
    chown "${host_uid}:${host_gid}" /target/release
  install -m 0755 "${work_dir}/corrosion" "${TARGET_DIR}/release/corrosion.tmp"
  mv "${TARGET_DIR}/release/corrosion.tmp" "${TARGET_DIR}/release/corrosion"
  install -m 0644 "${manifest_file}" "${TARGET_DIR}/release/corrosion-release.json"
  install -m 0644 \
    "${ROOT_DIR}/docs/design/corrosion-schema-v1.sql" \
    "${TARGET_DIR}/release/corrosion-schema-v1.sql"
  rm -rf "${work_dir}"
}

bake_workload_tarball() {
  local platform name image source_image save_image tar stamp image_id stamp_value temp_tar temp_stamp index
  platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
  mkdir -p "${WORKLOAD_STAMP_DIR}"
  for index in "${!WORKLOAD_NAMES[@]}"; do
    name="${WORKLOAD_NAMES[index]}"
    image="${WORKLOAD_IMAGES[index]}"
    source_image="${WORKLOAD_SOURCES[index]}"
    docker pull --platform "${platform}" "${source_image}"
    save_image="${image%@*}"
    if [ "${source_image}" != "${save_image}" ]; then
      docker tag "${source_image}" "${save_image}"
    fi
    tar="${CONTEXT_DIR}/${name}.tar"
    stamp="${WORKLOAD_STAMP_DIR}/${name}.stamp"
    image_id="$(docker image inspect --format '{{.Id}}' "${save_image}")"
    stamp_value="${platform} ${image} ${source_image} ${image_id}"
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
  done
}

build_machine_image() {
  local fingerprint
  fingerprint="$(machine_fingerprint)"
  docker build \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --label dev.ployz.dind.managed=true \
    --label "dev.ployz.dind.fingerprint=${fingerprint}" \
    --build-arg "BASE_IMAGE=${MACHINE_BASE_IMAGE_SOURCE}" \
    --build-arg "DOCKER_HUB_MIRROR=${DOCKER_HUB_MIRROR}" \
    --tag "${MACHINE_IMAGE}" \
    "${CONTEXT_DIR}"
}

verify_machine_tools() {
  docker run --rm \
    --platform "$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")" \
    --entrypoint /bin/sh \
    --volume "${TARGET_DIR}/release:/opt/ployz/artifacts:ro" \
    "${MACHINE_IMAGE}" \
    -c 'bpftool version >/dev/null && ufw version >/dev/null'
}

build_linux_artifacts
build_ebpf_bytecode
stage_corrosion

if [ "${mode}" = "full" ]; then
  bake_workload_tarball
  build_machine_image
  verify_machine_tools
  echo "DinD machine image built: ${MACHINE_IMAGE}"
  echo "Host-arch linux artifacts (volume-mount these at test time):"
else
  echo "Linux release artifacts built:"
fi

cat <<EOF
  ployzd:         ${TARGET_DIR}/release/ployzd
  ployz:          ${TARGET_DIR}/release/ployz
  ployz-ebpf-ctl: ${TARGET_DIR}/release/ployz-ebpf-ctl
  ployz-ebpf-tc:  ${TARGET_DIR}/release/ployz-ebpf-tc
  corrosion:      ${TARGET_DIR}/release/corrosion
EOF
