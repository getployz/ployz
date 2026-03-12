#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="${ROOT_DIR}"
OUTPUT_DIR=""

usage() {
  cat <<'EOF'
Usage:
  scripts/build-install-payload.sh --output PATH [--repo PATH]
EOF
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
  local version asset tmp_dir
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

  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "${tmp_dir}"' RETURN
  download "https://github.com/getployz/corrosion/releases/download/${version}/${asset}" "${tmp_dir}/${asset}"
  tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"
  install -m 0755 "${tmp_dir}/corrosion" "${output_dir}/bin/corrosion"
  printf 'CORROSION_VERSION=%s\n' "${version}" > "${output_dir}/metadata.env"
}

build_binaries() {
  cd "${REPO_DIR}"
  if [[ "$(uname -s)" == "Linux" ]]; then
    if [[ ! -f "${REPO_DIR}/ebpf/target/bpfel-unknown-none/release/ployz-ebpf-tc" ]]; then
      "${REPO_DIR}/scripts/install-ebpf-bytecode.sh"
    fi
    cargo build --release -p ployzd --features ebpf-native --bins
    cargo build --release -p ployz-gateway -p ployz-dns
    return
  fi

  cargo build --release -p ployzd --bins
  cargo build --release -p ployz-gateway -p ployz-dns
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

build_binaries

rm -rf "${OUTPUT_DIR}"
install -d "${OUTPUT_DIR}/bin" "${OUTPUT_DIR}/assets/systemd"
install -m 0755 "${REPO_DIR}/ployz.sh" "${OUTPUT_DIR}/ployz.sh"
install -m 0755 "${REPO_DIR}/target/release/ployz" "${OUTPUT_DIR}/bin/ployz"
install -m 0755 "${REPO_DIR}/target/release/ployzd" "${OUTPUT_DIR}/bin/ployzd"
install -m 0755 "${REPO_DIR}/target/release/ployz-gateway" "${OUTPUT_DIR}/bin/ployz-gateway"
install -m 0755 "${REPO_DIR}/target/release/ployz-dns" "${OUTPUT_DIR}/bin/ployz-dns"
install -m 0644 "${REPO_DIR}/packaging/systemd/ployzd.service" "${OUTPUT_DIR}/assets/systemd/ployzd.service"

install_corrosion "${OUTPUT_DIR}"

{
  printf 'GIT_REV=%s\n' "$(git -C "${REPO_DIR}" rev-parse --short HEAD 2>/dev/null || printf 'unknown')"
  printf 'PLATFORM=%s-%s\n' "$(uname -s | tr '[:upper:]' '[:lower:]')" "$(uname -m)"
} >> "${OUTPUT_DIR}/metadata.env"
