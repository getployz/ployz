#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${1:-${PLOYZ_EBPF_REPO:-getployz/ployz}}"

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
  echo "curl or wget is required to download eBPF bytecode" >&2
  exit 1
}

checksum_file() {
  local file_path=$1
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${file_path}" | awk '{print $1}'
    return
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${file_path}" | awk '{print $1}'
    return
  fi
  echo "sha256sum or shasum is required" >&2
  exit 1
}

version_file="${ROOT_DIR}/.ebpf-version"
if [[ ! -f "${version_file}" ]]; then
  echo "missing ${version_file}" >&2
  exit 1
fi
version="$(tr -d '[:space:]' < "${version_file}")"
if [[ -z "${version}" ]]; then
  echo "empty eBPF version in ${version_file}" >&2
  exit 1
fi

dest_dir="${ROOT_DIR}/crates/ebpf/target/bpfel-unknown-none/release"
dest_file="${dest_dir}/ployz-ebpf-tc"
stamp="${dest_dir}/.ebpf-release-version"

if [[ -f "${dest_file}" && -f "${stamp}" ]]; then
  installed="$(tr -d '[:space:]' < "${stamp}")"
  if [[ "${installed}" == "${version}" ]]; then
    echo "eBPF bytecode ${version} already present; skipping"
    exit 0
  fi
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

base_url="https://github.com/${REPO}/releases/download/ebpf-${version}"
download "${base_url}/checksums.txt" "${tmp_dir}/checksums.txt"
download "${base_url}/ployz-ebpf-tc" "${tmp_dir}/ployz-ebpf-tc"

expected="$(awk '$2 == "dist/ployz-ebpf-tc" || $2 == "ployz-ebpf-tc" { print $1; exit }' "${tmp_dir}/checksums.txt")"
if [[ -z "${expected}" ]]; then
  echo "missing checksum entry for ployz-ebpf-tc" >&2
  exit 1
fi
actual="$(checksum_file "${tmp_dir}/ployz-ebpf-tc")"
if [[ "${expected}" != "${actual}" ]]; then
  echo "checksum mismatch: expected ${expected}, got ${actual}" >&2
  exit 1
fi

mkdir -p "${dest_dir}"
cp "${tmp_dir}/ployz-ebpf-tc" "${dest_file}"
printf '%s\n' "${version}" > "${stamp}"
echo "installed eBPF bytecode ${version}"
