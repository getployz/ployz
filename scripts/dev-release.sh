#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

ARTIFACT_DIR="${PLOYZ_DEV_ARTIFACT_DIR:-${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}/release}"
OUTPUT_ROOT="${PLOYZ_DEV_RELEASE_DIR:-${ROOT_DIR}/dist/dev-releases}"
ARTIFACTS=(ployz ployzd ployz-ebpf-ctl ployz-ebpf-tc)

usage() {
  cat >&2 <<EOF
usage: scripts/dev-release.sh build

Builds a content-addressed local Linux release bundle and prints its directory.

env:
  PLOYZ_DIND_PLATFORM=linux/amd64   target platform (amd64 or arm64)
  PLOYZ_DEV_SKIP_BUILD=1            reuse artifacts in ${ARTIFACT_DIR}
  PLOYZ_DEV_RELEASE_DIR=...         bundle parent directory
EOF
}

if [ "${1:-}" != "build" ] || [ "$#" -ne 1 ]; then
  usage
  exit 1
fi

platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
case "${platform}" in
  linux/amd64)
    platform_slug="linux-amd64"
    ;;
  linux/arm64)
    platform_slug="linux-arm64"
    ;;
  *)
    echo "local release bundles support linux/amd64 and linux/arm64, got ${platform}" >&2
    exit 1
    ;;
esac
if [ "${PLOYZ_DEV_SKIP_BUILD:-0}" != "1" ]; then
  PLOYZ_DIND_PLATFORM="${platform}" \
    PLOYZ_DIND_CARGO_INCREMENTAL=1 \
    bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh" artifacts-only >&2
fi

for artifact in "${ARTIFACTS[@]}"; do
  if [ ! -f "${ARTIFACT_DIR}/${artifact}" ]; then
    echo "missing artifact: ${ARTIFACT_DIR}/${artifact}" >&2
    exit 1
  fi
done

content_hash="$({
  for artifact in "${ARTIFACTS[@]}"; do
    printf '%s %s\n' "${artifact}" "$(sha256_of "${ARTIFACT_DIR}/${artifact}")"
  done
  printf 'ployz.sh %s\n' "$(sha256_of "${ROOT_DIR}/scripts/ployz.sh")"
  printf 'platform %s\n' "${platform_slug}"
} | sha256_stdin)"
version="dev-${content_hash:0:16}"
remote_dir="/var/lib/ployz/dev-releases/${version}"
bundle_dir="${OUTPUT_ROOT}/${version}"
staging_dir="${bundle_dir}.tmp.$$"

rm -rf "${staging_dir}"
mkdir -p "${staging_dir}"
for artifact in "${ARTIFACTS[@]}"; do
  install -m 0755 "${ARTIFACT_DIR}/${artifact}" "${staging_dir}/${artifact}"
done
install -m 0755 "${ROOT_DIR}/scripts/ployz.sh" "${staging_dir}/ployz.sh"

{
  printf 'PLOYZ_VERSION=%s\n' "${version}"
  printf 'PLOYZ_RELEASE_TAG=%s\n' "${version}"
  printf 'PLOYZ_RELEASE_PLATFORM=%s\n' "${platform_slug}"
  for artifact in "${ARTIFACTS[@]}"; do
    case "${artifact}" in
      ployz) key=PLOYZ ;;
      ployzd) key=PLOYZD ;;
      ployz-ebpf-ctl) key=PLOYZ_EBPF_CTL ;;
      ployz-ebpf-tc) key=PLOYZ_EBPF_TC ;;
    esac
    printf '%s_URL=%s/%s\n' "${key}" "${remote_dir}" "${artifact}"
    printf '%s_SHA256=%s\n' "${key}" "$(sha256_of "${staging_dir}/${artifact}")"
  done
} > "${staging_dir}/release.env"

cat > "${staging_dir}/install.sh" <<EOF
#!/bin/sh
set -eu
PLOYZ_RELEASE_MANIFEST_URL=file://${remote_dir}/release.env exec ${remote_dir}/ployz.sh "\$@"
EOF
chmod 0755 "${staging_dir}/install.sh"

mkdir -p "${OUTPUT_ROOT}"
if [ -d "${bundle_dir}" ]; then
  rm -rf "${staging_dir}"
else
  mv "${staging_dir}" "${bundle_dir}"
fi
printf '%s\n' "${bundle_dir}"
