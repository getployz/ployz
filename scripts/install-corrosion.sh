#!/usr/bin/env bash
set -euo pipefail

prefix="${1:-target/tools}"
repo="${CORROSION_REPO:-superfly/corrosion}"

download() {
  local url="$1"
  local dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "${dest}" "${url}"
    return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -qO "${dest}" "${url}"
    return
  fi
  echo "curl or wget is required to download corrosion" >&2
  exit 1
}

version_file=".corrosion-version"
if [[ ! -f "${version_file}" ]]; then
  echo "missing ${version_file}" >&2
  exit 1
fi
version="$(tr -d '[:space:]' < "${version_file}")"
if [[ -z "${version}" ]]; then
  echo "empty corrosion version in ${version_file}" >&2
  exit 1
fi

install_path="${prefix}/bin/corrosion"
version_stamp="${prefix}/.corrosion-release-version"
if [[ -x "${install_path}" && -f "${version_stamp}" ]]; then
  installed_version="$(tr -d '[:space:]' < "${version_stamp}")"
  if [[ "${installed_version}" == "${version}" ]]; then
    echo "corrosion ${version} already installed at ${install_path}; skipping"
    exit 0
  fi
fi

os="$(uname -s)"
arch="$(uname -m)"
case "${os}" in
  Darwin) asset_os="apple-darwin" ;;
  Linux) asset_os="unknown-linux-gnu" ;;
  *)
    echo "unsupported platform: ${os}/${arch}" >&2
    exit 1
    ;;
esac
case "${arch}" in
  x86_64|amd64) asset_arch="x86_64" ;;
  aarch64|arm64) asset_arch="aarch64" ;;
  *)
    echo "unsupported platform: ${os}/${arch}" >&2
    exit 1
    ;;
esac

if [[ "${os}" == "Darwin" && "${asset_arch}" == "x86_64" ]]; then
  echo "corrosion ${version} has no darwin x86_64 release asset; use arm64 macOS or Linux CI" >&2
  exit 1
fi

asset="corrosion-${asset_arch}-${asset_os}.tar.gz"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

url="https://github.com/${repo}/releases/download/${version}/${asset}"
download "${url}" "${tmp_dir}/${asset}"
tar -xzf "${tmp_dir}/${asset}" -C "${tmp_dir}"
binary="$(find "${tmp_dir}" -type f -name corrosion -perm -111 | head -n 1)"
if [[ -z "${binary}" ]]; then
  echo "archive ${asset} did not contain executable corrosion" >&2
  exit 1
fi

install -d "${prefix}/bin"
install -m 0755 "${binary}" "${install_path}"
printf '%s\n' "${version}" > "${version_stamp}"
if [[ "${os}" == "Darwin" ]]; then
  xattr -d com.apple.quarantine "${install_path}" 2>/dev/null || true
fi
echo "installed corrosion ${install_path} (${version} for ${os}/${arch})"
