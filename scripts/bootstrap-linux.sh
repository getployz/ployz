#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'EOF'
Linux bootstrap shim for host system installs.

Usage:
  scripts/bootstrap-linux.sh --artifacts-dir PATH [options]
  scripts/bootstrap-linux.sh --artifacts-url URL [options]
EOF
}

ARTIFACTS_DIR=""
ARTIFACTS_URL=""
FORCE_DOWNLOAD=0
RUNTIME="host"
SERVICE_MODE="system"
SKIP_START=0
WORK_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --artifacts-dir)
      ARTIFACTS_DIR=${2:-}
      shift 2
      ;;
    --artifacts-url)
      ARTIFACTS_URL=${2:-}
      shift 2
      ;;
    --force-download)
      FORCE_DOWNLOAD=1
      shift
      ;;
    --runtime)
      RUNTIME=${2:-}
      shift 2
      ;;
    --service-mode)
      SERVICE_MODE=${2:-}
      shift 2
      ;;
    --skip-start|--refresh-only|--apt-proxy|--prefix|--data-dir)
      if [[ "$1" == "--skip-start" || "$1" == "--refresh-only" ]]; then
        [[ "$1" == "--skip-start" ]] && SKIP_START=1
        shift
      else
        shift 2
      fi
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

[[ "${RUNTIME}" == "host" && "${SERVICE_MODE}" == "system" ]] || {
  printf 'bootstrap shim only supports --runtime host --service-mode system\n' >&2
  exit 1
}

if [[ -n "${ARTIFACTS_DIR}" && -n "${ARTIFACTS_URL}" ]]; then
  printf 'use either --artifacts-dir or --artifacts-url, not both\n' >&2
  exit 1
fi
if [[ -z "${ARTIFACTS_DIR}" && -z "${ARTIFACTS_URL}" ]]; then
  printf 'one of --artifacts-dir or --artifacts-url is required\n' >&2
  exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

if [[ -n "${ARTIFACTS_URL}" ]]; then
  archive_path="${WORK_DIR}/payload.tgz"
  if command -v curl >/dev/null 2>&1; then
    curl_args=(-fsSL)
    [[ ${FORCE_DOWNLOAD} -eq 1 ]] || :
    curl "${curl_args[@]}" "${ARTIFACTS_URL}" -o "${archive_path}"
  else
    wget -qO "${archive_path}" "${ARTIFACTS_URL}"
  fi
  mkdir -p "${WORK_DIR}/payload"
  tar -xzf "${archive_path}" -C "${WORK_DIR}/payload"
  ARTIFACTS_DIR="${WORK_DIR}/payload"
fi

"${ROOT_DIR}/ployz.sh" install \
  --source payload \
  --payload-dir "${ARTIFACTS_DIR}" \
  --runtime host \
  --service-mode system

if [[ ${SKIP_START} -eq 1 ]]; then
  printf 'warning: legacy --skip-start is no longer supported by the shim\n' >&2
fi
