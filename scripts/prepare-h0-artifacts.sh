#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${PLOYZ_ACCEPTANCE_TARGET_DIR:-/tmp/ployz-linux-amd64-target}"
EBPF_TARGET_DIR="${PLOYZ_EBPF_TARGET_DIR:-/tmp/ployz-rust-ebpf-source-target}"
NATS_SERVER_VERSION="${PLOYZ_ACCEPTANCE_NATS_SERVER_VERSION:-2.14.2}"

need_file() {
  local path=$1
  local label=$2
  [[ -f "${path}" ]] || {
    echo "${label} does not exist: ${path}" >&2
    exit 1
  }
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
  echo "curl or wget is required to download ${url}" >&2
  exit 1
}

need_linux_amd64_artifact() {
  local path=$1
  local label=$2
  need_file "${path}" "${label}"
  if command -v file >/dev/null 2>&1; then
    file "${path}" | grep -Eq 'ELF 64-bit .* x86-64' || {
      file "${path}" >&2
      echo "${label} must be a Linux x86_64 ELF artifact: ${path}" >&2
      exit 1
    }
  fi
}

build_linux_artifacts() {
  if [[ "${PLOYZ_ACCEPTANCE_SKIP_BUILD:-0}" == "1" ]]; then
    return
  fi

  if [[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]]; then
    cargo build \
      --release \
      --target-dir "${TARGET_DIR}" \
      -p ployzctl \
      -p ployzd \
      -p ployz-keeper \
      -p ployz-ebpf-ctl
    return
  fi

  command -v docker >/dev/null 2>&1 || {
    echo "docker is required to build Linux amd64 H0 artifacts from this host" >&2
    exit 1
  }

  docker run --rm --platform linux/amd64 \
    -v "${ROOT_DIR}:/work" \
    -v "${TARGET_DIR}:/target" \
    -w /work \
    rust:1.91-bookworm \
    cargo build --release --target-dir /target \
      -p ployzctl \
      -p ployzd \
      -p ployz-keeper \
      -p ployz-ebpf-ctl
}

resolve_nats_server() {
  if [[ -n "${PLOYZ_ACCEPTANCE_NATS_SERVER:-}" ]]; then
    printf '%s\n' "${PLOYZ_ACCEPTANCE_NATS_SERVER}"
    return
  fi

  local tool_dir="${TARGET_DIR}/h0-tools"
  local server="${tool_dir}/nats-server-linux-amd64"
  if [[ -x "${server}" ]]; then
    printf '%s\n' "${server}"
    return
  fi

  mkdir -p "${tool_dir}"
  local archive="${tool_dir}/nats-server-v${NATS_SERVER_VERSION}-linux-amd64.tar.gz.$$"
  local unpack_parent="${tool_dir}/nats-server-unpack.$$"
  local unpack_dir="${unpack_parent}/nats-server-v${NATS_SERVER_VERSION}-linux-amd64"
  local tmp_server="${server}.$$"
  download \
    "https://github.com/nats-io/nats-server/releases/download/v${NATS_SERVER_VERSION}/nats-server-v${NATS_SERVER_VERSION}-linux-amd64.tar.gz" \
    "${archive}"
  mkdir -p "${unpack_parent}"
  tar -xzf "${archive}" -C "${unpack_parent}"
  cp "${unpack_dir}/nats-server" "${tmp_server}"
  chmod 0755 "${tmp_server}"
  mv "${tmp_server}" "${server}"
  rm -rf "${archive}" "${unpack_parent}"
  printf '%s\n' "${server}"
}

resolve_ebpf_bytecode() {
  if [[ -n "${PLOYZ_ACCEPTANCE_EBPF_BYTECODE:-}" ]]; then
    printf '%s\n' "${PLOYZ_ACCEPTANCE_EBPF_BYTECODE}"
    return
  fi
  PLOYZ_EBPF_TARGET_DIR="${EBPF_TARGET_DIR}" "${ROOT_DIR}/scripts/build-ebpf-bytecode.sh" | tail -n 1
}

build_linux_artifacts

ployzctl_bin="${TARGET_DIR}/release/ployzctl"
ployzd_bin="${TARGET_DIR}/release/ployzd"
keeper_bin="${TARGET_DIR}/release/ployz-keeper"
ebpf_ctl_bin="${TARGET_DIR}/release/ployz-ebpf-ctl"
nats_server_bin="$(resolve_nats_server)"
ebpf_bytecode="$(resolve_ebpf_bytecode)"

need_linux_amd64_artifact "${ployzctl_bin}" "ployzctl"
need_linux_amd64_artifact "${ployzd_bin}" "ployzd"
need_linux_amd64_artifact "${keeper_bin}" "ployz-keeper"
need_linux_amd64_artifact "${ebpf_ctl_bin}" "ployz-ebpf-ctl"
need_linux_amd64_artifact "${nats_server_bin}" "nats-server"
need_file "${ebpf_bytecode}" "eBPF bytecode"
cargo run -q -p ployz-ebpf-ctl -- validate "${ebpf_bytecode}"

cat <<EOF
export PLOYZ_ACCEPTANCE_TARGET_DIR="${TARGET_DIR}"
export PLOYZ_ACCEPTANCE_PLOYZCTL="${ployzctl_bin}"
export PLOYZ_ACCEPTANCE_PLOYZD="${ployzd_bin}"
export PLOYZ_ACCEPTANCE_KEEPER="${keeper_bin}"
export PLOYZ_ACCEPTANCE_EBPF_CTL="${ebpf_ctl_bin}"
export PLOYZ_ACCEPTANCE_EBPF_BYTECODE="${ebpf_bytecode}"
export PLOYZ_ACCEPTANCE_NATS_SERVER="${nats_server_bin}"

scripts/hetzner-two-node-acceptance.sh up --run-id <id>
EOF
