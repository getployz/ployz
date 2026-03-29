#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_DIR="${ROOT_DIR}"
OUTPUT_DIR=""
TARGET_PLATFORM=""
BUILD_PROFILE="${PLOYZ_PAYLOAD_BUILD_PROFILE:-release}"

usage() {
  cat <<'EOF'
Usage:
  scripts/package-release-payload.sh --output-dir PATH [--repo PATH] [--target-platform OS/ARCH] [--profile debug|release]
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-dir)
      OUTPUT_DIR=${2:-}
      shift 2
      ;;
    --repo)
      REPO_DIR=${2:-}
      shift 2
      ;;
    --target-platform)
      TARGET_PLATFORM=${2:-}
      shift 2
      ;;
    --profile)
      BUILD_PROFILE=${2:-}
      shift 2
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

[[ -n "${OUTPUT_DIR}" ]] || { usage >&2; exit 1; }
[[ -n "${TARGET_PLATFORM}" ]] || { usage >&2; exit 1; }

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

payload_dir="${tmp_dir}/payload"
asset_name="$(bash "${REPO_DIR}/ployz.sh" internal payload-asset-name --target-platform "${TARGET_PLATFORM}")"

bash "${REPO_DIR}/scripts/build-install-payload.sh" \
  --repo "${REPO_DIR}" \
  --output "${payload_dir}" \
  --target-platform "${TARGET_PLATFORM}" \
  --profile "${BUILD_PROFILE}"

mkdir -p "${OUTPUT_DIR}"
tar -czf "${OUTPUT_DIR}/${asset_name}" -C "${payload_dir}" .
printf '%s\n' "${OUTPUT_DIR}/${asset_name}"
