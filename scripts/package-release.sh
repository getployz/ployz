#!/usr/bin/env bash
set -euo pipefail

# Build and stage GitHub release assets for the current Docker host platform.
# The public installer reads the generated ployz-release-<platform>.env file.

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

case "${release_tag}" in
  *[!A-Za-z0-9._-]*)
    echo "release version contains unsupported characters: ${version}" >&2
    exit 1
    ;;
esac

platform="$(docker_platform "${PLOYZ_RELEASE_PLATFORM:-}")"
platform_slug="${platform//\//-}"
target_dir="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
artifact_dir="${PLOYZ_RELEASE_ARTIFACT_DIR:-${target_dir}/release}"
dist_dir="${PLOYZ_RELEASE_DIST_DIR:-${ROOT_DIR}/dist/releases/${release_tag}}"
asset_base_url="${PLOYZ_RELEASE_ASSET_BASE_URL:-https://github.com/getployz/ployz/releases/download/${release_tag}}"

if [ "${PLOYZ_RELEASE_SKIP_BUILD:-0}" != "1" ]; then
  bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh"
fi

required_artifacts=(
  ployzd
  ployzctl
  ployz-keeper
  ployz-ebpf-ctl
  ployz-ebpf-tc
)

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

  install -m "${mode}" "${artifact_dir}/${name}" "${dist_dir}/${asset}"
  printf '%s\n' "${asset}"
}

ployzd_asset="$(copy_asset ployzd 0755)"
ployzctl_asset="$(copy_asset ployzctl 0755)"
keeper_asset="$(copy_asset ployz-keeper 0755)"
ebpf_ctl_asset="$(copy_asset ployz-ebpf-ctl 0755)"
ebpf_tc_asset="$(copy_asset ployz-ebpf-tc 0644)"
manifest_asset="ployz-release-${platform_slug}.env"
manifest_path="${dist_dir}/${manifest_asset}"

write_manifest_pair() {
  local key="$1"
  local asset="$2"
  local path="${dist_dir}/${asset}"

  printf '%s_URL=%s/%s\n' "${key}" "${asset_base_url}" "${asset}"
  printf '%s_SHA256=%s\n' "${key}" "$(sha256_of "${path}")"
}

{
  printf 'PLOYZ_VERSION=%s\n' "${semver}"
  printf 'PLOYZ_RELEASE_TAG=%s\n' "${release_tag}"
  printf 'PLOYZ_RELEASE_PLATFORM=%s\n' "${platform_slug}"
  write_manifest_pair PLOYZCTL "${ployzctl_asset}"
  write_manifest_pair PLOYZ_KEEPER "${keeper_asset}"
  write_manifest_pair PLOYZD "${ployzd_asset}"
  write_manifest_pair PLOYZ_EBPF_CTL "${ebpf_ctl_asset}"
  write_manifest_pair PLOYZ_EBPF_TC "${ebpf_tc_asset}"
} > "${manifest_path}"

cat <<EOF
Release assets staged in ${dist_dir}
Upload these files to ${release_tag}:
  ${manifest_asset}
  ${ployzctl_asset}
  ${keeper_asset}
  ${ployzd_asset}
  ${ebpf_ctl_asset}
  ${ebpf_tc_asset}
EOF
