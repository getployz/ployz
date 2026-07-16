#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"
# shellcheck source=config/railpack-pins.env
source "${ROOT_DIR}/config/railpack-pins.env"

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
    nats_sha256="b3e7b14eb10c895fd90c2dacdb6b65bd3208adcc9524dd7689ba2c1024e6b97a"
    railpack_archive_name="${RAILPACK_AMD64_ARCHIVE}"
    railpack_archive_sha256="${RAILPACK_AMD64_ARCHIVE_SHA256}"
    ;;
  linux/arm64)
    platform_slug="linux-arm64"
    nats_sha256="15fd0c3438e7178e5316e63be68373ad581c8d78db26e649113aa303b74e5e58"
    railpack_archive_name="${RAILPACK_ARM64_ARCHIVE}"
    railpack_archive_sha256="${RAILPACK_ARM64_ARCHIVE_SHA256}"
    ;;
  *)
    echo "local release bundles support linux/amd64 and linux/arm64, got ${platform}" >&2
    exit 1
    ;;
esac
nats_version="2.14.2"
nats_url="https://github.com/nats-io/nats-server/releases/download/v${nats_version}/nats-server-v${nats_version}-${platform_slug}.tar.gz"
railpack_version="${RAILPACK_VERSION}"
railpack_url="https://github.com/railwayapp/railpack/releases/download/${railpack_version}/${railpack_archive_name}"

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

work_dir="$(mktemp -d)"
cleanup() {
  rm -rf "${work_dir}"
}
trap cleanup EXIT
railpack_archive="${work_dir}/${railpack_archive_name}"
curl --fail --location --silent --show-error "${railpack_url}" --output "${railpack_archive}"
actual_railpack_sha256="$(sha256_of "${railpack_archive}")"
if [ "${actual_railpack_sha256}" != "${railpack_archive_sha256}" ]; then
  echo "Railpack release archive has SHA-256 ${actual_railpack_sha256}, expected ${railpack_archive_sha256}" >&2
  exit 1
fi
tar -xzf "${railpack_archive}" -C "${work_dir}" railpack
railpack_source="${work_dir}/railpack"

content_hash="$({
  for artifact in "${ARTIFACTS[@]}"; do
    printf '%s %s\n' "${artifact}" "$(sha256_of "${ARTIFACT_DIR}/${artifact}")"
  done
  printf 'ployz.sh %s\n' "$(sha256_of "${ROOT_DIR}/scripts/ployz.sh")"
  printf 'platform %s\n' "${platform_slug}"
  printf 'nats-version %s\n' "${nats_version}"
  printf 'nats-url %s\n' "${nats_url}"
  printf 'nats-sha256 %s\n' "${nats_sha256}"
  printf 'railpack-version %s\n' "${railpack_version}"
  printf 'railpack-sha256 %s\n' "$(sha256_of "${railpack_source}")"
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
install -m 0755 "${railpack_source}" "${staging_dir}/railpack"

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
  printf 'PLOYZ_RAILPACK_VERSION=%s\n' "${railpack_version}"
  printf 'PLOYZ_RAILPACK_URL=%s/%s\n' "${remote_dir}" railpack
  printf 'PLOYZ_RAILPACK_SHA256=%s\n' "$(sha256_of "${staging_dir}/railpack")"
  printf 'PLOYZ_NATS_SERVER_VERSION=%s\n' "${nats_version}"
  printf 'PLOYZ_NATS_SERVER_URL=%s\n' "${nats_url}"
  printf 'PLOYZ_NATS_SERVER_SHA256=%s\n' "${nats_sha256}"
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
