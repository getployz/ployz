#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="${ROOT_DIR}"
OUTPUT_DIR=""
TARGET_PLATFORM=""
DEFAULT_BUILDER_IMAGE="ployz-payload-builder:bookworm"
BUILDER_IMAGE="${PLOYZ_PAYLOAD_BUILDER_IMAGE:-${DEFAULT_BUILDER_IMAGE}}"
BUILDER_DOCKERFILE="${PLOYZ_PAYLOAD_BUILDER_DOCKERFILE:-${ROOT_DIR}/packaging/e2e/payload-builder.Dockerfile}"
BUILD_PROFILE="${PLOYZ_PAYLOAD_BUILD_PROFILE:-release}"

usage() {
  cat <<'EOF'
Usage:
  scripts/build-install-payload.sh --output PATH [--repo PATH] [--target-platform OS/ARCH] [--profile debug|release]
EOF
}

current_os() {
  printf '%s' "$(uname -s | tr '[:upper:]' '[:lower:]')"
}

current_arch() {
  case "$(uname -m)" in
    x86_64|amd64)
      printf 'amd64'
      ;;
    aarch64|arm64)
      printf 'arm64'
      ;;
    *)
      printf '%s' "$(uname -m)"
      ;;
  esac
}

current_platform() {
  printf '%s/%s' "$(current_os)" "$(current_arch)"
}

output_dir_parent() {
  local output_dir=$1
  local parent
  parent="$(dirname "${output_dir}")"
  mkdir -p "${parent}"
  (cd "${parent}" && pwd)
}

cache_key() {
  local repo_dir=$1
  if command -v shasum >/dev/null 2>&1; then
    printf '%s' "${repo_dir}" | shasum -a 256 | awk '{print substr($1, 1, 12)}'
    return
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "${repo_dir}" | sha256sum | awk '{print substr($1, 1, 12)}'
    return
  fi
  printf '%s' "$(basename "${repo_dir}")"
}

build_linux_payload_in_docker() {
  local output_dir=$1
  local repo_dir=$2
  local target_platform=$3
  local build_profile=$4
  local repo_abs output_parent_abs output_name target_cache_dir owner_uid owner_gid
  local cache_suffix cargo_registry_volume cargo_git_volume target_volume corrosion_cache_volume

  repo_abs="$(cd "${repo_dir}" && pwd)"
  output_parent_abs="$(output_dir_parent "${output_dir}")"
  output_name="$(basename "${output_dir}")"
  cache_suffix="$(cache_key "${repo_abs}")-${target_platform//\//-}-${build_profile}"
  target_cache_dir="/cargo-target"
  owner_uid="$(id -u)"
  owner_gid="$(id -g)"
  cargo_registry_volume="ployz-payload-${cache_suffix}-cargo-registry"
  cargo_git_volume="ployz-payload-${cache_suffix}-cargo-git"
  target_volume="ployz-payload-${cache_suffix}-target"
  corrosion_cache_volume="ployz-payload-corrosion-${target_platform//\//-}"

  ensure_builder_image

  docker run --rm \
    --platform "${target_platform}" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    -e PLOYZ_PAYLOAD_BUILD_INTERNAL=1 \
    -e PLOYZ_PAYLOAD_BUILD_PROFILE="${build_profile}" \
    -e CARGO_TARGET_DIR="${target_cache_dir}" \
    -e PLOYZ_PAYLOAD_OWNER_UID="${owner_uid}" \
    -e PLOYZ_PAYLOAD_OWNER_GID="${owner_gid}" \
    -e PLOYZ_PAYLOAD_CORROSION_CACHE_DIR=/corrosion-cache \
    -v "${cargo_registry_volume}:/cargo/registry" \
    -v "${cargo_git_volume}:/cargo/git" \
    -v "${target_volume}:${target_cache_dir}" \
    -v "${corrosion_cache_volume}:/corrosion-cache" \
    -v "${repo_abs}:/repo" \
    -v "${output_parent_abs}:/out" \
    -w /repo \
    "${BUILDER_IMAGE}" \
    bash -c "
      set -euo pipefail
      export PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
      bash /repo/scripts/build-install-payload.sh \
        --repo /repo \
        --output /out/${output_name} \
        --target-platform ${target_platform} \
        --profile ${build_profile}
      chown -R \"${owner_uid}:${owner_gid}\" /out/${output_name}
      if [[ -d /repo/ebpf/target/bpfel-unknown-none/release ]]; then
        chown -R \"${owner_uid}:${owner_gid}\" /repo/ebpf/target/bpfel-unknown-none/release
      fi
    "
}

ensure_builder_image() {
  if docker image inspect "${BUILDER_IMAGE}" >/dev/null 2>&1; then
    return
  fi

  if [[ "${BUILDER_IMAGE}" != "${DEFAULT_BUILDER_IMAGE}" ]]; then
    printf 'builder image not found: %s\n' "${BUILDER_IMAGE}" >&2
    exit 1
  fi

  docker build \
    -f "${BUILDER_DOCKERFILE}" \
    -t "${BUILDER_IMAGE}" \
    "${ROOT_DIR}/packaging/e2e"
}

download() {
  local url=$1
  local dest=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "${dest}" "${url}"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -qO "${dest}" "${url}"
    return
  fi
  printf 'curl or wget is required to download %s\n' "${url}" >&2
  exit 1
}

install_corrosion() {
  local output_dir=$1
  local version asset tmp_dir cache_dir cache_archive cache_binary
  version="$(tr -d '[:space:]' < "${REPO_DIR}/.corrosion-version")"
  case "$(uname -s):$(uname -m)" in
    Darwin:arm64)
      asset="corrosion-aarch64-apple-darwin.tar.gz"
      ;;
    Darwin:x86_64)
      asset="corrosion-x86_64-apple-darwin.tar.gz"
      ;;
    Linux:aarch64|Linux:arm64)
      asset="corrosion-aarch64-unknown-linux-gnu.tar.gz"
      ;;
    Linux:x86_64|Linux:amd64)
      asset="corrosion-x86_64-unknown-linux-gnu.tar.gz"
      ;;
    *)
      printf 'unsupported corrosion platform: %s/%s\n' "$(uname -s)" "$(uname -m)" >&2
      exit 1
      ;;
  esac

  cache_dir="${PLOYZ_PAYLOAD_CORROSION_CACHE_DIR:-}"
  if [[ -n "${cache_dir}" ]]; then
    mkdir -p "${cache_dir}"
    cache_archive="${cache_dir}/${version}-${asset}"
    cache_binary="${cache_dir}/${version}-${asset%.tar.gz}"
    if [[ ! -f "${cache_archive}" ]]; then
      download "https://github.com/getployz/corrosion/releases/download/${version}/${asset}" "${cache_archive}"
    fi
    if [[ ! -x "${cache_binary}" ]]; then
      tmp_dir="$(mktemp -d)"
      trap 'rm -rf "${tmp_dir}"' RETURN
      tar -xzf "${cache_archive}" -C "${tmp_dir}"
      install -m 0755 "${tmp_dir}/corrosion" "${cache_binary}"
    fi
    install -m 0755 "${cache_binary}" "${output_dir}/bin/corrosion"
    printf 'CORROSION_VERSION=%s\n' "${version}" > "${output_dir}/metadata.env"
    return
  fi

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "${tmp_dir}"' RETURN
  download "https://github.com/getployz/corrosion/releases/download/${version}/${asset}" "${tmp_dir}/${asset}"
  tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"
  install -m 0755 "${tmp_dir}/corrosion" "${output_dir}/bin/corrosion"
  printf 'CORROSION_VERSION=%s\n' "${version}" > "${output_dir}/metadata.env"
}

build_binaries() {
  local cargo_args
  cargo_args=()
  if [[ "${BUILD_PROFILE}" == "release" ]]; then
    cargo_args+=(--release)
  fi

  cd "${REPO_DIR}"
  if [[ "$(uname -s)" == "Linux" ]]; then
    if [[ ! -f "${REPO_DIR}/ebpf/target/bpfel-unknown-none/release/ployz-ebpf-tc" ]]; then
      "${REPO_DIR}/scripts/install-ebpf-bytecode.sh"
    fi
    cargo build "${cargo_args[@]}" -p ployzd --features ebpf-native --bins
    cargo build "${cargo_args[@]}" -p ployz-gateway -p ployz-dns
    return
  fi

  cargo build "${cargo_args[@]}" -p ployzd --bins
  cargo build "${cargo_args[@]}" -p ployz-gateway -p ployz-dns
}

binary_build_dir() {
  local profile_dir
  profile_dir="${BUILD_PROFILE}"
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    printf '%s/%s' "${CARGO_TARGET_DIR}" "${profile_dir}"
    return
  fi
  printf '%s/target/%s' "${REPO_DIR}" "${profile_dir}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      OUTPUT_DIR=${2:-}
      shift 2
      ;;
    --repo)
      REPO_DIR=${2:-}
      shift 2
      ;;
    --target-platform)
      TARGET_PLATFORM=${2:-}
      shift 2
      ;;
    --profile)
      BUILD_PROFILE=${2:-}
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

[[ -n "${OUTPUT_DIR}" ]] || { usage >&2; exit 1; }
[[ -n "${TARGET_PLATFORM}" ]] || TARGET_PLATFORM="$(current_platform)"
case "${BUILD_PROFILE}" in
  debug|release) ;;
  *)
    printf 'unsupported build profile: %s\n' "${BUILD_PROFILE}" >&2
    exit 1
    ;;
esac

if [[ -z "${PLOYZ_PAYLOAD_BUILD_INTERNAL:-}" && "${TARGET_PLATFORM}" != "$(current_platform)" ]]; then
  case "${TARGET_PLATFORM}" in
    linux/amd64|linux/arm64)
      build_linux_payload_in_docker "${OUTPUT_DIR}" "${REPO_DIR}" "${TARGET_PLATFORM}" "${BUILD_PROFILE}"
      exit 0
      ;;
    *)
      printf 'unsupported target platform: %s\n' "${TARGET_PLATFORM}" >&2
      exit 1
      ;;
  esac
fi

build_binaries

rm -rf "${OUTPUT_DIR}"
install -d "${OUTPUT_DIR}/bin" "${OUTPUT_DIR}/assets/systemd"
install -m 0755 "${REPO_DIR}/ployz.sh" "${OUTPUT_DIR}/ployz.sh"
install -m 0755 "$(binary_build_dir)/ployz" "${OUTPUT_DIR}/bin/ployz"
install -m 0755 "$(binary_build_dir)/ployzd" "${OUTPUT_DIR}/bin/ployzd"
install -m 0755 "$(binary_build_dir)/ployz-gateway" "${OUTPUT_DIR}/bin/ployz-gateway"
install -m 0755 "$(binary_build_dir)/ployz-dns" "${OUTPUT_DIR}/bin/ployz-dns"
install -m 0644 "${REPO_DIR}/packaging/systemd/ployzd.service" "${OUTPUT_DIR}/assets/systemd/ployzd.service"

install_corrosion "${OUTPUT_DIR}"

{
  printf 'GIT_REV=%s\n' "$(git -C "${REPO_DIR}" rev-parse --short HEAD 2>/dev/null || printf 'unknown')"
  printf 'PLATFORM=%s\n' "${TARGET_PLATFORM}"
  printf 'PROFILE=%s\n' "${BUILD_PROFILE}"
} >> "${OUTPUT_DIR}/metadata.env"
