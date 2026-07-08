#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${PLOYZ_DEV_ARTIFACT_DIR:-${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}/release}"
ARTIFACTS=(ployzd ployz-keeper ployz-ebpf-ctl ployz-ebpf-tc)

usage() {
  cat >&2 <<EOF
usage: scripts/dev-push-substrate.sh root@server [root@server...]

Builds linux dev artifacts locally, copies them to each server, then asks the
staged ployz-keeper to install them from a local manifest.

env:
  PLOYZ_DEV_SKIP_BUILD=1      reuse ${ARTIFACT_DIR}
  PLOYZ_DIND_PLATFORM=...     default linux/amd64
EOF
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ] || [ "$#" -eq 0 ]; then
  usage
  exit 0
fi

if [ "${PLOYZ_DEV_SKIP_BUILD:-0}" != "1" ]; then
  PLOYZ_DIND_PLATFORM="${PLOYZ_DIND_PLATFORM:-linux/amd64}" \
    bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh"
fi

for artifact in "${ARTIFACTS[@]}"; do
  if [ ! -f "${ARTIFACT_DIR}/${artifact}" ]; then
    echo "missing artifact: ${ARTIFACT_DIR}/${artifact}" >&2
    exit 1
  fi
done

push_host() {
  local host="$1"
  local remote_dir="/tmp/ployz-dev-push-$RANDOM-$$"

  echo "==> ${host}"
  ssh "${host}" "rm -rf '${remote_dir}' && mkdir -p '${remote_dir}'"
  tar -C "${ARTIFACT_DIR}" -czf - "${ARTIFACTS[@]}" \
    | ssh "${host}" "tar -xzf - -C '${remote_dir}'"

  ssh "${host}" \
    "PLOYZ_REMOTE_DIR='${remote_dir}' bash -se" \
    <<'REMOTE'
set -euo pipefail

sha256_of() {
  sha256sum "$1" | cut -d ' ' -f 1
}

chmod 0755 "${PLOYZ_REMOTE_DIR}/ployz-keeper"
install -m 0755 "${PLOYZ_REMOTE_DIR}/ployz-keeper" /usr/local/bin/ployz-keeper
cat > "${PLOYZ_REMOTE_DIR}/release.env" <<EOF
PLOYZ_VERSION=dev-local
PLOYZD_URL=${PLOYZ_REMOTE_DIR}/ployzd
PLOYZD_SHA256=$(sha256_of "${PLOYZ_REMOTE_DIR}/ployzd")
PLOYZ_EBPF_CTL_URL=${PLOYZ_REMOTE_DIR}/ployz-ebpf-ctl
PLOYZ_EBPF_CTL_SHA256=$(sha256_of "${PLOYZ_REMOTE_DIR}/ployz-ebpf-ctl")
PLOYZ_EBPF_TC_URL=${PLOYZ_REMOTE_DIR}/ployz-ebpf-tc
PLOYZ_EBPF_TC_SHA256=$(sha256_of "${PLOYZ_REMOTE_DIR}/ployz-ebpf-tc")
PLOYZ_NATS_SERVER_VERSION=dev-local
PLOYZ_NATS_SERVER_URL=/tmp/ployz-dev-no-nats-server.tar.gz
PLOYZ_NATS_SERVER_SHA256=0000000000000000000000000000000000000000000000000000000000000000
EOF

"${PLOYZ_REMOTE_DIR}/ployz-keeper" substrate-update --manifest-file "${PLOYZ_REMOTE_DIR}/release.env"
rm -rf "${PLOYZ_REMOTE_DIR}"
REMOTE
}

pids=()
for host in "$@"; do
  push_host "${host}" &
  pids+=("$!")
done

status=0
for pid in "${pids[@]}"; do
  if ! wait "${pid}"; then
    status=1
  fi
done

exit "${status}"
