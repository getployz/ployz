#!/usr/bin/env bash
set -euo pipefail

# Build and stage GitHub release assets for one platform.
# The public installer reads the generated ployz-release-<platform>.env file.
#
# Linux platforms (linux/amd64, linux/arm64) stage the full machine artifact
# set built by scripts/build-dind-machine-image.sh through Docker. macOS
# platforms (darwin/amd64, darwin/arm64) stage the unified local operator CLI.
# Host Runner commands compile in and fail cleanly off Linux at runtime. Select
# the platform with PLOYZ_RELEASE_PLATFORM, e.g.:
#
#   PLOYZ_RELEASE_PLATFORM=darwin/arm64 scripts/package-release.sh

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

version="${PLOYZ_RELEASE_VERSION:-v0.0.1-alpha.1}"
case "${version}" in
  v*)
    release_tag="${version}"
    semver="${version#v}"
    ;;
  *)
    release_tag="v${version}"
    semver="${version}"
    ;;
esac
validate_release_semver "release version" "${version}" "${semver}"
case "${release_tag}" in
  *[!A-Za-z0-9._-]*)
    echo "release version contains unsupported characters: ${version}" >&2
    exit 1
    ;;
esac

platform="$(docker_platform "${PLOYZ_RELEASE_PLATFORM:-}")"
platform_os="${platform%%/*}"
platform_arch="${platform##*/}"
platform_slug="${platform//\//-}"
target_dir="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
dist_dir="${PLOYZ_RELEASE_DIST_DIR:-${ROOT_DIR}/dist/releases/${release_tag}}"
asset_base_url="${PLOYZ_RELEASE_ASSET_BASE_URL:-https://github.com/getployz/ployz/releases/download/${release_tag}}"

case "${platform_os}" in
  linux)
    artifact_dir="${PLOYZ_RELEASE_ARTIFACT_DIR:-${target_dir}/release}"
    required_artifacts=(
      ployzd
      ployz
      ployz-ebpf-ctl
      ployz-ebpf-tc
      corrosion
      corrosion-schema-v1.sql
    )
    command -v python3 >/dev/null 2>&1 || {
      echo "python3 is required to read corrosion-release.json" >&2
      exit 1
    }
    if ! corrosion_embedded_version="$(python3 \
      "${ROOT_DIR}/scripts/read-corrosion-release.py" \
      "${ROOT_DIR}/corrosion-release.json" \
      "${platform_slug}" \
      embedded_version)"; then
      exit 1
    fi
    if [ "${PLOYZ_RELEASE_SKIP_BUILD:-0}" != "1" ]; then
      PLOYZ_DIND_PLATFORM="${platform}" \
        bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh" artifacts-only
    fi
    ;;
  darwin)
    case "${platform_arch}" in
      amd64)
        darwin_triple="x86_64-apple-darwin"
        ;;
      arm64)
        darwin_triple="aarch64-apple-darwin"
        ;;
      *)
        echo "unsupported darwin release architecture: ${platform_arch} (supported: amd64, arm64)" >&2
        exit 1
        ;;
    esac
    release_cargo_target_dir="${PLOYZ_RELEASE_CARGO_TARGET_DIR:-${CARGO_TARGET_DIR:-${ROOT_DIR}/target}}"
    export CARGO_TARGET_DIR="${release_cargo_target_dir}"
    artifact_dir="${PLOYZ_RELEASE_ARTIFACT_DIR:-${release_cargo_target_dir}/${darwin_triple}/release}"
    required_artifacts=(
      ployz
    )
    if [ "${PLOYZ_RELEASE_SKIP_BUILD:-0}" != "1" ]; then
      if [ "$(uname -s)" != "Darwin" ]; then
        echo "darwin release artifacts must be built on macOS (host is $(uname -s))" >&2
        exit 1
      fi
      rustup target add "${darwin_triple}"
      cargo build --release --package ployz --target "${darwin_triple}"
    fi
    ;;
  *)
    echo "unsupported release platform: ${platform} (supported: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64)" >&2
    exit 1
    ;;
esac

for artifact in "${required_artifacts[@]}"; do
  if [ ! -f "${artifact_dir}/${artifact}" ]; then
    echo "missing release artifact: ${artifact_dir}/${artifact}" >&2
    exit 1
  fi
done

mkdir -p "${dist_dir}"

copy_asset() {
  local name="$1"
  local mode="$2"
  local asset="${name}-${platform_slug}"

  copy_asset_as "${name}" "${asset}" "${mode}"
}

copy_asset_as() {
  local source_name="$1"
  local asset="$2"
  local mode="$3"

  install -m "${mode}" "${artifact_dir}/${source_name}" "${dist_dir}/${asset}"
  printf '%s\n' "${asset}"
}

manifest_asset="ployz-release-${platform_slug}.env"
manifest_path="${dist_dir}/${manifest_asset}"

write_manifest_pair() {
  local key="$1"
  local asset="$2"
  local path="${dist_dir}/${asset}"

  printf '%s_URL=%s/%s\n' "${key}" "${asset_base_url}" "${asset}"
  printf '%s_SHA256=%s\n' "${key}" "$(sha256_of "${path}")"
}

staged_assets=("${manifest_asset}")

ployz_asset="$(copy_asset ployz 0755)"
staged_assets+=("${ployz_asset}")

if [ "${platform_os}" = "linux" ]; then
  ployzd_asset="$(copy_asset ployzd 0755)"
  ebpf_ctl_asset="$(copy_asset ployz-ebpf-ctl 0755)"
  ebpf_tc_asset="$(copy_asset ployz-ebpf-tc 0644)"
  corrosion_asset="$(copy_asset corrosion 0755)"
  corrosion_schema_asset="$(copy_asset_as corrosion-schema-v1.sql "corrosion-schema-v1-${platform_slug}.sql" 0644)"
  staged_assets+=(
    "${ployzd_asset}"
    "${ebpf_ctl_asset}"
    "${ebpf_tc_asset}"
    "${corrosion_asset}"
    "${corrosion_schema_asset}"
  )
fi

{
  printf 'PLOYZ_VERSION=%s\n' "${semver}"
  printf 'PLOYZ_RELEASE_TAG=%s\n' "${release_tag}"
  printf 'PLOYZ_RELEASE_PLATFORM=%s\n' "${platform_slug}"
  write_manifest_pair PLOYZ "${ployz_asset}"
  # Machine substrate artifacts are Linux-only; darwin manifests carry just
  # the unified ployz binary, and Host Runner commands are Linux-gated.
  if [ "${platform_os}" = "linux" ]; then
    write_manifest_pair PLOYZD "${ployzd_asset}"
    write_manifest_pair PLOYZ_EBPF_CTL "${ebpf_ctl_asset}"
    write_manifest_pair PLOYZ_EBPF_TC "${ebpf_tc_asset}"
    printf 'PLOYZ_CORROSION_EMBEDDED_VERSION=%s\n' "${corrosion_embedded_version}"
    write_manifest_pair PLOYZ_CORROSION "${corrosion_asset}"
    write_manifest_pair PLOYZ_CORROSION_SCHEMA "${corrosion_schema_asset}"
  fi
} > "${manifest_path}"

echo "Release assets staged in ${dist_dir}"
echo "Upload these files to ${release_tag}:"
for asset in "${staged_assets[@]}"; do
  echo "  ${asset}"
done
