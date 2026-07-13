#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS=(ployz ployzd ployz-ebpf-ctl ployz-ebpf-tc ployz.sh install.sh release.env)

usage() {
  cat >&2 <<EOF
usage: scripts/dev-push-substrate.sh [--release DIR] root@server [root@server...]

Builds (or reuses) a local release bundle, stages it persistently on each host,
then runs the explicit substrate-update command. This does not promote the
cluster's machine join release template.
EOF
}

release_dir=""
if [ "${1:-}" = "--release" ]; then
  [ "$#" -ge 3 ] || { usage; exit 1; }
  release_dir="$2"
  shift 2
fi
if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ] || [ "$#" -eq 0 ]; then
  usage
  exit 0
fi

if [ -z "${release_dir}" ]; then
  release_dir="$(bash "${ROOT_DIR}/scripts/dev-release.sh" build)"
fi
release_dir="$(cd "${release_dir}" && pwd)"

for artifact in "${ARTIFACTS[@]}"; do
  [ -f "${release_dir}/${artifact}" ] || {
    echo "invalid local release: missing ${release_dir}/${artifact}" >&2
    exit 1
  }
done
manifest_value() {
  awk -F= -v key="$1" '$1 == key { print substr($0, length(key) + 2); exit }' "${release_dir}/release.env"
}

version="$(manifest_value PLOYZ_VERSION)"
case "${version}" in
  dev-[0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) ;;
  *) echo "invalid local release version: ${version:-<empty>}" >&2; exit 1 ;;
esac
remote_dir="/var/lib/ployz/dev-releases/${version}"
if [ "$(manifest_value PLOYZ_RELEASE_TAG)" != "${version}" ]; then
  echo "invalid local release: manifest identity does not match ${version}" >&2
  exit 1
fi
for artifact in ployz ployzd ployz-ebpf-ctl ployz-ebpf-tc; do
  case "${artifact}" in
    ployz) key=PLOYZ ;;
    ployzd) key=PLOYZD ;;
    ployz-ebpf-ctl) key=PLOYZ_EBPF_CTL ;;
    ployz-ebpf-tc) key=PLOYZ_EBPF_TC ;;
  esac
  [ "$(manifest_value "${key}_URL")" = "${remote_dir}/${artifact}" ] || {
    echo "invalid local release: ${key}_URL is not canonical" >&2
    exit 1
  }
done

push_host() {
  local host="$1" remote_staging="${remote_dir}.tmp.$$"

  echo "==> ${host}"
  ssh "${host}" "rm -rf '${remote_staging}' && install -d -m 0755 '${remote_staging}'"
  tar -C "${release_dir}" -czf - "${ARTIFACTS[@]}" \
    | ssh "${host}" "tar -xzf - -C '${remote_staging}' && chmod -R u=rwX,go=rX '${remote_staging}' && (mv -T '${remote_staging}' '${remote_dir}' 2>/dev/null || rm -rf '${remote_staging}')"
  ssh "${host}" "install -m 0755 '${remote_dir}/ployz' /usr/local/bin/ployz && '${remote_dir}/ployz' host substrate-update --manifest-file '${remote_dir}/release.env'"
}

pids=()
for host in "$@"; do
  push_host "${host}" &
  pids+=("$!")
done

status=0
for pid in "${pids[@]}"; do
  wait "${pid}" || status=1
done
exit "${status}"
