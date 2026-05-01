#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="${ROOT_DIR}"
OUTPUT_DIR=""
TARGET_PLATFORM=""
BUILDER_IMAGE_FALLBACK="rust:1-bookworm"
BUILDER_IMAGE="${PLOYZ_PAYLOAD_BUILDER_IMAGE:-}"
BUILD_PROFILE="${PLOYZ_PAYLOAD_BUILD_PROFILE:-release}"
BUILD_INPUT_PATHS=(
  Cargo.toml
  Cargo.lock
  .nats-version
  ployz.sh
  crates
  ebpf
  packaging
  scripts
)

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

repo_cache_root() {
  local repo_dir=$1
  local cache_root
  if cache_root="$(git -C "${repo_dir}" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
    :
  elif cache_root="$(git -C "${repo_dir}" rev-parse --git-common-dir 2>/dev/null)"; then
    if [[ "${cache_root}" != /* ]]; then
      cache_root="$(cd "${repo_dir}" && cd "${cache_root}" && pwd -P)"
    fi
  else
    cache_root="$(cd "${repo_dir}" && pwd -P)"
  fi
  printf '%s' "${cache_root}"
}

cache_key() {
  local repo_dir=$1
  local cache_root
  cache_root="$(repo_cache_root "${repo_dir}")"
  if command -v shasum >/dev/null 2>&1; then
    printf '%s' "${cache_root}" | shasum -a 256 | awk '{print substr($1, 1, 12)}'
    return
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    printf '%s' "${cache_root}" | sha256sum | awk '{print substr($1, 1, 12)}'
    return
  fi
  printf '%s' "$(basename "${cache_root}")"
}

hashfiles_single() {
  local path=$1
  local file_hash
  file_hash="$(hash_file "${path}")"
  printf '%s' "${file_hash}" | xxd -r -p | shasum -a 256 | awk '{print $1}'
}

resolve_builder_image() {
  local target_platform=$1
  local repo_dir=$2
  local arch_suffix dockerfile candidate hash

  if [[ -n "${BUILDER_IMAGE}" ]]; then
    return
  fi

  dockerfile="${repo_dir}/Dockerfile.payload-builder"
  if [[ ! -f "${dockerfile}" ]]; then
    BUILDER_IMAGE="${BUILDER_IMAGE_FALLBACK}"
    return
  fi

  arch_suffix="${target_platform##*/}"
  hash="$(hashfiles_single "${dockerfile}")"
  candidate="ghcr.io/getployz/ployz-payload-builder:bookworm-${hash}-${arch_suffix}"

  if docker image inspect "${candidate}" >/dev/null 2>&1; then
    printf 'using prebuilt builder image (local): %s\n' "${candidate}" >&2
    BUILDER_IMAGE="${candidate}"
    return
  fi

  if docker pull --quiet "${candidate}" >/dev/null 2>&1; then
    printf 'using prebuilt builder image (pulled): %s\n' "${candidate}" >&2
    BUILDER_IMAGE="${candidate}"
    return
  fi

  printf 'prebuilt builder image unavailable, falling back to %s (run `docker login ghcr.io` to enable)\n' "${BUILDER_IMAGE_FALLBACK}" >&2
  BUILDER_IMAGE="${BUILDER_IMAGE_FALLBACK}"
}

build_linux_payload_in_docker() {
  local output_dir=$1
  local repo_dir=$2
  local target_platform=$3
  local build_profile=$4
  local repo_abs output_parent_abs output_name target_cache_dir owner_uid owner_gid
  local cache_suffix cargo_registry_mount cargo_git_mount target_mount
  local host_cache_dir host_registry host_git host_target

  resolve_builder_image "${target_platform}" "${repo_dir}"

  repo_abs="$(cd "${repo_dir}" && pwd)"
  output_parent_abs="$(output_dir_parent "${output_dir}")"
  output_name="$(basename "${output_dir}")"
  cache_suffix="$(cache_key "${repo_abs}")-${target_platform//\//-}-${build_profile}"
  target_cache_dir="/cargo-target"
  owner_uid="$(id -u)"
  owner_gid="$(id -g)"

  # When PLOYZ_PAYLOAD_HOST_CACHE_DIR is set, bind-mount its subdirs into the
  # builder so the cargo registry / git / target dirs live on the host. CI uses
  # this with actions/cache so the cache survives across runs. Otherwise fall
  # back to docker named volumes (good enough for local dev where they persist
  # between invocations).
  host_cache_dir="${PLOYZ_PAYLOAD_HOST_CACHE_DIR:-}"
  if [[ -n "${host_cache_dir}" ]]; then
    host_registry="${host_cache_dir}/${cache_suffix}/cargo-registry"
    host_git="${host_cache_dir}/${cache_suffix}/cargo-git"
    host_target="${host_cache_dir}/${cache_suffix}/target"
    mkdir -p "${host_registry}" "${host_git}" "${host_target}"
    cargo_registry_mount="${host_registry}:/cargo/registry"
    cargo_git_mount="${host_git}:/cargo/git"
    target_mount="${host_target}:${target_cache_dir}"
  else
    cargo_registry_mount="ployz-payload-${cache_suffix}-cargo-registry:/cargo/registry"
    cargo_git_mount="ployz-payload-${cache_suffix}-cargo-git:/cargo/git"
    target_mount="ployz-payload-${cache_suffix}-target:${target_cache_dir}"
  fi

  docker run --rm \
    --platform "${target_platform}" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    -e PLOYZ_PAYLOAD_BUILD_INTERNAL=1 \
    -e PLOYZ_PAYLOAD_BUILD_PROFILE="${build_profile}" \
    -e PLOYZ_PAYLOAD_BUILD_FINGERPRINT="${BUILD_FINGERPRINT:-}" \
    -e CARGO_TARGET_DIR="${target_cache_dir}" \
    -e PLOYZ_PAYLOAD_OWNER_UID="${owner_uid}" \
    -e PLOYZ_PAYLOAD_OWNER_GID="${owner_gid}" \
    -e PLOYZ_PAYLOAD_CHOWN_CACHE="${host_cache_dir:+1}" \
    -v "${cargo_registry_mount}" \
    -v "${cargo_git_mount}" \
    -v "${target_mount}" \
    -v "${repo_abs}:/repo" \
    -v "${output_parent_abs}:/out" \
    -w /repo \
    "${BUILDER_IMAGE}" \
    bash -c "
      set -euo pipefail
      export PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
      if ! command -v cmake >/dev/null 2>&1 \\
         || ! command -v pkg-config >/dev/null 2>&1 \\
         || ! dpkg -s libclang-dev >/dev/null 2>&1; then
        apt-get update >/dev/null
        apt-get install -y --no-install-recommends cmake libclang-dev pkg-config >/dev/null
        rm -rf /var/lib/apt/lists/*
      fi
      bash /repo/scripts/build-install-payload.sh \
        --repo /repo \
        --output /out/${output_name} \
        --target-platform ${target_platform} \
        --profile ${build_profile}
      chown -R \"${owner_uid}:${owner_gid}\" /out/${output_name}
      if [[ -n \"\${PLOYZ_PAYLOAD_CHOWN_CACHE:-}\" ]]; then
        chown -R \"${owner_uid}:${owner_gid}\" /cargo /cargo-target
      fi
      if [[ -d /repo/ebpf/target ]]; then
        chown -R \"${owner_uid}:${owner_gid}\" /repo/ebpf/target
      fi
    "
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

cached_download() {
  local url=$1
  local dest=$2
  local cache_dir=$3
  local cache_name=$4
  local cache_path tmp_path

  mkdir -p "${cache_dir}"
  cache_path="${cache_dir}/${cache_name}"

  if [[ -f "${cache_path}" ]]; then
    cp "${cache_path}" "${dest}"
    printf 'archive cache hit %s\n' "${cache_name}" >&2
    return
  fi

  tmp_path="${cache_path}.tmp.$$"
  printf 'archive download start %s\n' "${cache_name}" >&2
  download "${url}" "${tmp_path}"
  mv "${tmp_path}" "${cache_path}"
  cp "${cache_path}" "${dest}"
  printf 'archive download complete %s\n' "${cache_name}" >&2
}

hash_file() {
  local path=$1
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{print $1}'
    return
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{print $1}'
    return
  fi
  printf 'missing shasum/sha256sum for %s\n' "${path}" >&2
  exit 1
}

hash_build_inputs_without_git() {
  local tmp_file input rel_path
  tmp_file="$(mktemp)"
  trap 'rm -f "${tmp_file}"' RETURN

  for input in "${BUILD_INPUT_PATHS[@]}"; do
    rel_path="${input#./}"
    if [[ -f "${REPO_DIR}/${rel_path}" ]]; then
      printf 'FILE %s %s\n' "${rel_path}" "$(hash_file "${REPO_DIR}/${rel_path}")" >> "${tmp_file}"
      continue
    fi

    if [[ -d "${REPO_DIR}/${rel_path}" ]]; then
      while IFS= read -r path; do
        printf 'FILE %s %s\n' "${path}" "$(hash_file "${REPO_DIR}/${path}")" >> "${tmp_file}"
      done < <(cd "${REPO_DIR}" && find "${rel_path}" -type f | LC_ALL=C sort)
      continue
    fi

    printf 'MISSING %s\n' "${rel_path}" >> "${tmp_file}"
  done

  hash_file "${tmp_file}"
}

repo_build_fingerprint() {
  local head_rev dirty_paths tmp_file path abs_path

  if ! git -C "${REPO_DIR}" rev-parse --show-toplevel >/dev/null 2>&1; then
    printf 'nogit:%s' "$(hash_build_inputs_without_git)"
    return
  fi

  head_rev="$(git -C "${REPO_DIR}" rev-parse HEAD 2>/dev/null || printf 'unknown')"
  dirty_paths="$(
    {
      git -C "${REPO_DIR}" diff --name-only -- "${BUILD_INPUT_PATHS[@]}"
      git -C "${REPO_DIR}" diff --cached --name-only -- "${BUILD_INPUT_PATHS[@]}"
      git -C "${REPO_DIR}" ls-files --others --exclude-standard -- "${BUILD_INPUT_PATHS[@]}"
    } | sed '/^$/d' | LC_ALL=C sort -u
  )"

  if [[ -z "${dirty_paths}" ]]; then
    printf 'git:%s' "${head_rev}"
    return
  fi

  tmp_file="$(mktemp)"
  trap 'rm -f "${tmp_file}"' RETURN
  printf 'HEAD %s\n' "${head_rev}" > "${tmp_file}"

  while IFS= read -r path; do
    [[ -n "${path}" ]] || continue
    abs_path="${REPO_DIR}/${path}"
    if [[ -f "${abs_path}" ]]; then
      printf 'FILE %s %s\n' "${path}" "$(hash_file "${abs_path}")" >> "${tmp_file}"
      continue
    fi
    printf 'MISSING %s\n' "${path}" >> "${tmp_file}"
  done <<< "${dirty_paths}"

  printf 'git:%s-dirty:%s' "${head_rev}" "$(hash_file "${tmp_file}")"
}

metadata_value() {
  local metadata_path=$1
  local key=$2
  awk -F= -v key="${key}" '$1 == key { print substr($0, length(key) + 2); exit }' "${metadata_path}"
}

payload_is_fresh() {
  local metadata_path=$1
  local build_fingerprint=$2
  local metadata_fingerprint metadata_platform metadata_profile

  [[ -f "${metadata_path}" ]] || return 1
  [[ -f "${OUTPUT_DIR}/ployz.sh" ]] || return 1
  [[ -f "${OUTPUT_DIR}/bin/ployzctl" ]] || return 1
  [[ -f "${OUTPUT_DIR}/bin/ployzd" ]] || return 1
  [[ -f "${OUTPUT_DIR}/bin/ployz-gateway" ]] || return 1
  [[ -f "${OUTPUT_DIR}/bin/ployz-dns" ]] || return 1
  [[ -f "${OUTPUT_DIR}/bin/nats-server" ]] || return 1

  metadata_fingerprint="$(metadata_value "${metadata_path}" BUILD_FINGERPRINT)"
  metadata_platform="$(metadata_value "${metadata_path}" PLATFORM)"
  metadata_profile="$(metadata_value "${metadata_path}" PROFILE)"

  [[ "${metadata_fingerprint}" == "${build_fingerprint}" ]] || return 1
  [[ "${metadata_platform}" == "${TARGET_PLATFORM}" ]] || return 1
  [[ "${metadata_profile}" == "${BUILD_PROFILE}" ]] || return 1
}

install_nats_server() {
  local output_dir=$1
  local version os arch asset tmp_dir cache_dir archive_url extracted_binary
  version="$(tr -d '[:space:]' < "${REPO_DIR}/.nats-version")"
  case "$(uname -s)" in
    Darwin)
      os="darwin"
      ;;
    Linux)
      os="linux"
      ;;
    *)
      printf 'unsupported nats-server platform: %s/%s\n' "$(uname -s)" "$(uname -m)" >&2
      exit 1
      ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64)
      arch="amd64"
      ;;
    aarch64|arm64)
      arch="arm64"
      ;;
    *)
      printf 'unsupported nats-server platform: %s/%s\n' "$(uname -s)" "$(uname -m)" >&2
      exit 1
      ;;
  esac

  asset="nats-server-${version}-${os}-${arch}.tar.gz"
  cache_dir="$(repo_cache_root "${REPO_DIR}")/.payload-cache/nats-server"
  archive_url="https://github.com/nats-io/nats-server/releases/download/${version}/${asset}"
  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "${tmp_dir}"' RETURN
  cached_download "${archive_url}" "${tmp_dir}/${asset}" "${cache_dir}" "${version}-${asset}"
  tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"
  extracted_binary="$(find "${tmp_dir}" -type f -name nats-server -perm -111 | head -n 1)"
  if [[ -z "${extracted_binary}" ]]; then
    printf 'archive %s did not contain executable nats-server\n' "${asset}" >&2
    exit 1
  fi
  copy_file "${extracted_binary}" "${output_dir}/bin/nats-server" 0755
  printf 'NATS_SERVER_VERSION=%s\n' "${version}" >> "${output_dir}/metadata.env"
}

copy_file() {
  local src=$1
  local dest=$2
  local mode=$3

  rm -f "${dest}"
  cat "${src}" > "${dest}"
  chmod "${mode}" "${dest}"
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
    cargo build "${cargo_args[@]}" -p ployzctl -p ployz-gateway -p ployz-dns
    return
  fi

  cargo build "${cargo_args[@]}" -p ployzctl -p ployzd --bins
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

BUILD_FINGERPRINT="${PLOYZ_PAYLOAD_BUILD_FINGERPRINT:-$(repo_build_fingerprint)}"
METADATA_PATH="${OUTPUT_DIR}/metadata.env"
if payload_is_fresh "${METADATA_PATH}" "${BUILD_FINGERPRINT}"; then
  printf 'payload cache hit profile=%s platform=%s\n' "${BUILD_PROFILE}" "${TARGET_PLATFORM}"
  exit 0
fi

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

mkdir -p "${OUTPUT_DIR}"
install -d "${OUTPUT_DIR}/bin"
install_nats_server "${OUTPUT_DIR}"
build_binaries

output_parent="$(dirname "${OUTPUT_DIR}")"
output_name="$(basename "${OUTPUT_DIR}")"
tmp_output_dir="$(mktemp -d "${output_parent}/.${output_name}.tmp.XXXXXX")"
trap 'rm -rf "${tmp_output_dir}"' EXIT

install -d "${tmp_output_dir}/bin" "${tmp_output_dir}/assets/systemd"
copy_file "${REPO_DIR}/ployz.sh" "${tmp_output_dir}/ployz.sh" 0755
copy_file "$(binary_build_dir)/ployzctl" "${tmp_output_dir}/bin/ployzctl" 0755
copy_file "$(binary_build_dir)/ployzd" "${tmp_output_dir}/bin/ployzd" 0755
copy_file "$(binary_build_dir)/ployz-gateway" "${tmp_output_dir}/bin/ployz-gateway" 0755
copy_file "$(binary_build_dir)/ployz-dns" "${tmp_output_dir}/bin/ployz-dns" 0755
copy_file "${REPO_DIR}/packaging/systemd/ployzd.service" "${tmp_output_dir}/assets/systemd/ployzd.service" 0644
copy_file "${OUTPUT_DIR}/bin/nats-server" "${tmp_output_dir}/bin/nats-server" 0755

{
  cat "${OUTPUT_DIR}/metadata.env"
  printf 'GIT_REV=%s\n' "$(git -C "${REPO_DIR}" rev-parse --short HEAD 2>/dev/null || printf 'unknown')"
  printf 'PLATFORM=%s\n' "${TARGET_PLATFORM}"
  printf 'PROFILE=%s\n' "${BUILD_PROFILE}"
  printf 'BUILD_FINGERPRINT=%s\n' "${BUILD_FINGERPRINT}"
} >> "${tmp_output_dir}/metadata.env"

rm -rf "${OUTPUT_DIR}"
mv "${tmp_output_dir}" "${OUTPUT_DIR}"
trap - EXIT
