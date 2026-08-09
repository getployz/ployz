#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

usage() {
  cat >&2 <<'EOF'
usage: scripts/verify-release-assets.sh <vX.Y.Z> [--assets-dir <dir>] [--channel <name>] [--write-channel <path>] [--print-channel]

Verifies release manifests and referenced assets before a release tag is promoted
through a mutable channel file.
EOF
}

release_tag="${1:-}"
if [ -z "${release_tag}" ]; then
  usage
  exit 1
fi
shift

case "${release_tag}" in
  v*)
    semver="${release_tag#v}"
    ;;
  *)
    echo "release tag must start with v: ${release_tag}" >&2
    exit 1
    ;;
esac
validate_release_semver "release tag" "${release_tag}" "${semver}"
case "${release_tag}" in
  *[!A-Za-z0-9._-]*)
    echo "release tag contains unsupported characters: ${release_tag}" >&2
    exit 1
    ;;
esac
release_base_url="https://github.com/getployz/ployz/releases/download/${release_tag}"
command -v python3 >/dev/null 2>&1 || {
  echo "python3 is required to read corrosion-release.json" >&2
  exit 1
}
expected_corrosion_embedded_version="$(python3 \
  "${ROOT_DIR}/scripts/read-corrosion-release.py" \
  "${ROOT_DIR}/corrosion-release.json" \
  linux-amd64 \
  embedded_version)"

assets_dir="${PLOYZ_RELEASE_VERIFY_DIR:-}"
channel_name="alpha"
write_channel=""
print_channel=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --assets-dir)
      if [ "$#" -lt 2 ]; then
        usage
        exit 1
      fi
      assets_dir="$2"
      shift 2
      ;;
    --channel)
      if [ "$#" -lt 2 ]; then
        usage
        exit 1
      fi
      channel_name="$2"
      shift 2
      ;;
    --write-channel)
      if [ "$#" -lt 2 ]; then
        usage
        exit 1
      fi
      write_channel="$2"
      shift 2
      ;;
    --print-channel)
      print_channel=1
      shift
      ;;
    -*)
      echo "unknown release verification argument: $1" >&2
      exit 1
      ;;
    *)
      usage
      exit 1
      ;;
  esac
done

case "${channel_name}" in
  "" | *[!A-Za-z0-9._-]*)
    echo "channel contains unsupported characters: ${channel_name}" >&2
    exit 1
    ;;
esac

tmp_dir=""
cleanup() {
  if [ -n "${tmp_dir}" ]; then
    rm -rf "${tmp_dir}"
  fi
}
trap cleanup EXIT

if [ -z "${assets_dir}" ]; then
  command -v gh >/dev/null || {
    echo "gh is required when --assets-dir is not provided" >&2
    exit 1
  }
  tmp_dir="$(mktemp -d)"
  assets_dir="${tmp_dir}"
  gh release download "${release_tag}" --dir "${assets_dir}" --pattern '*'
fi

if [ ! -d "${assets_dir}" ]; then
  echo "release assets directory does not exist: ${assets_dir}" >&2
  exit 1
fi

manifest_value() {
  local manifest="$1"
  local key="$2"
  awk -F= -v key="$key" '$1 == key { print substr($0, length(key) + 2); exit }' "${manifest}"
}

require_value() {
  local manifest="$1"
  local key="$2"
  local value
  value="$(manifest_value "${manifest}" "${key}")"
  if [ -z "${value}" ]; then
    echo "release manifest ${manifest} is missing ${key}" >&2
    exit 1
  fi
  printf '%s\n' "${value}"
}

verify_manifest_identity() {
  local manifest="$1"
  local platform="$2"
  local manifest_tag
  local manifest_version
  local manifest_platform

  manifest_tag="$(require_value "${manifest}" PLOYZ_RELEASE_TAG)"
  manifest_version="$(require_value "${manifest}" PLOYZ_VERSION)"
  manifest_platform="$(require_value "${manifest}" PLOYZ_RELEASE_PLATFORM)"

  if [ "${manifest_tag}" != "${release_tag}" ]; then
    echo "release manifest ${manifest} has PLOYZ_RELEASE_TAG=${manifest_tag}, expected ${release_tag}" >&2
    exit 1
  fi
  if [ "${manifest_version}" != "${semver}" ]; then
    echo "release manifest ${manifest} has PLOYZ_VERSION=${manifest_version}, expected ${semver}" >&2
    exit 1
  fi
  if [ "${manifest_platform}" != "${platform}" ]; then
    echo "release manifest ${manifest} has PLOYZ_RELEASE_PLATFORM=${manifest_platform}, expected ${platform}" >&2
    exit 1
  fi
}

verify_asset_pair() {
  local manifest="$1"
  local key="$2"
  local expected_asset_name="$3"
  local url
  local expected_sha
  local asset_path
  local actual_sha

  url="$(require_value "${manifest}" "${key}_URL")"
  expected_sha="$(require_value "${manifest}" "${key}_SHA256")"
  asset_path="${assets_dir}/${expected_asset_name}"
  expected_url="${release_base_url}/${expected_asset_name}"

  if [ "${url}" != "${expected_url}" ]; then
    echo "release manifest ${manifest} has ${key}_URL=${url}, expected ${expected_url}" >&2
    exit 1
  fi

  if [ ! -f "${asset_path}" ]; then
    echo "release manifest ${manifest} references missing asset ${expected_asset_name}" >&2
    exit 1
  fi

  actual_sha="$(sha256_of "${asset_path}")"
  if [ "${actual_sha}" != "${expected_sha}" ]; then
    echo "release asset ${expected_asset_name} has SHA-256 ${actual_sha}, expected ${expected_sha}" >&2
    exit 1
  fi
}

verify_platform() {
  local platform="$1"
  shift
  local manifest="${assets_dir}/ployz-release-${platform}.env"

  if [ ! -f "${manifest}" ]; then
    echo "missing release manifest: ployz-release-${platform}.env" >&2
    exit 1
  fi

  verify_manifest_identity "${manifest}" "${platform}"
  for key in "$@"; do
    verify_asset_pair "${manifest}" "${key%%:*}" "${key#*:}-${platform}"
  done
}

verify_corrosion_assets() {
  local platform="$1"
  local manifest="${assets_dir}/ployz-release-${platform}.env"
  local embedded_version
  embedded_version="$(require_value "${manifest}" PLOYZ_CORROSION_EMBEDDED_VERSION)"
  if [ "${embedded_version}" != "${expected_corrosion_embedded_version}" ]; then
    echo "release manifest ${manifest} has PLOYZ_CORROSION_EMBEDDED_VERSION=${embedded_version}, expected ${expected_corrosion_embedded_version}" >&2
    exit 1
  fi
  verify_asset_pair \
    "${manifest}" \
    PLOYZ_CORROSION_SCHEMA \
    "corrosion-schema-v1-${platform}.sql"
}

verify_platform linux-amd64 PLOYZ:ployz PLOYZD:ployzd PLOYZ_EBPF_CTL:ployz-ebpf-ctl PLOYZ_EBPF_TC:ployz-ebpf-tc PLOYZ_CORROSION:corrosion
verify_corrosion_assets linux-amd64
verify_platform linux-arm64 PLOYZ:ployz PLOYZD:ployzd PLOYZ_EBPF_CTL:ployz-ebpf-ctl PLOYZ_EBPF_TC:ployz-ebpf-tc PLOYZ_CORROSION:corrosion
verify_corrosion_assets linux-arm64
verify_platform darwin-amd64 PLOYZ:ployz
verify_platform darwin-arm64 PLOYZ:ployz

channel_content() {
  cat <<EOF
PLOYZ_CHANNEL=${channel_name}
PLOYZ_RELEASE_TAG=${release_tag}
PLOYZ_VERSION=${semver}
PLOYZ_RELEASE_BASE_URL=${release_base_url}
EOF
}

if [ -n "${write_channel}" ]; then
  mkdir -p "$(dirname "${write_channel}")"
  channel_content > "${write_channel}"
fi

if [ "${print_channel}" -eq 1 ]; then
  channel_content
fi

echo "verified release assets for ${release_tag} in ${assets_dir}" >&2
