#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT_MANIFEST="${ROOT_DIR}/Cargo.toml"
EBPF_MANIFEST="${ROOT_DIR}/ebpf/Cargo.toml"
ROOT_LOCKFILE="${ROOT_DIR}/Cargo.lock"
EBPF_LOCKFILE="${ROOT_DIR}/ebpf/Cargo.lock"
BUMP_LEVEL=""
TARGET_VERSION=""

usage() {
  cat <<'EOF'
Usage:
  scripts/update-workspace-version.sh [--root PATH] (--bump patch|minor|major | --set VERSION)
EOF
}

ensure_file_contains() {
  local path=$1
  local pattern=$2

  if ! grep -Eq "${pattern}" "${path}"; then
    printf 'pattern %s not found in %s\n' "${pattern}" "${path}" >&2
    exit 1
  fi
}

set_version_line() {
  local path=$1
  local section_pattern=$2
  local version=$3

  perl -0pi -e "s/(${section_pattern})[^\"]+(\"\\n)/\${1}${version}\${2}/s" "${path}"
}

read_version_from_manifest() {
  local manifest_path=$1

  bash "${ROOT_DIR}/scripts/read-workspace-version.sh" --manifest "${manifest_path}"
}

validate_semver() {
  local version=$1

  if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'expected semantic version X.Y.Z, got %s\n' "${version}" >&2
    exit 1
  fi
}

bump_version() {
  local version=$1
  local level=$2
  local major minor patch

  IFS='.' read -r major minor patch <<< "${version}"

  case "${level}" in
    patch)
      patch=$((patch + 1))
      ;;
    minor)
      minor=$((minor + 1))
      patch=0
      ;;
    major)
      major=$((major + 1))
      minor=0
      patch=0
      ;;
    *)
      printf 'unknown bump level: %s\n' "${level}" >&2
      exit 1
      ;;
  esac

  printf '%s.%s.%s\n' "${major}" "${minor}" "${patch}"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --root)
      ROOT_DIR=${2:-}
      ROOT_MANIFEST="${ROOT_DIR}/Cargo.toml"
      EBPF_MANIFEST="${ROOT_DIR}/ebpf/Cargo.toml"
      ROOT_LOCKFILE="${ROOT_DIR}/Cargo.lock"
      EBPF_LOCKFILE="${ROOT_DIR}/ebpf/Cargo.lock"
      shift 2
      ;;
    --bump)
      BUMP_LEVEL=${2:-}
      shift 2
      ;;
    --set)
      TARGET_VERSION=${2:-}
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

if [[ -n "${BUMP_LEVEL}" && -n "${TARGET_VERSION}" ]]; then
  printf 'specify either --bump or --set, not both\n' >&2
  exit 1
fi

if [[ -z "${BUMP_LEVEL}" && -z "${TARGET_VERSION}" ]]; then
  usage >&2
  exit 1
fi

ensure_file_contains "${ROOT_MANIFEST}" '^\[workspace\.package\]$'
ensure_file_contains "${EBPF_MANIFEST}" '^\[package\]$'

current_version="$(read_version_from_manifest "${ROOT_MANIFEST}")"
validate_semver "${current_version}"

if [[ -n "${TARGET_VERSION}" ]]; then
  validate_semver "${TARGET_VERSION}"
else
  TARGET_VERSION="$(bump_version "${current_version}" "${BUMP_LEVEL}")"
fi

set_version_line "${ROOT_MANIFEST}" '\[workspace\.package\]\s*version = "' "${TARGET_VERSION}"
set_version_line "${EBPF_MANIFEST}" '\[package\]\s*name = "ployz-ebpf"\s*version = "' "${TARGET_VERSION}"

if [[ -f "${ROOT_LOCKFILE}" ]]; then
  cargo metadata --format-version 1 --manifest-path "${ROOT_MANIFEST}" >/dev/null
fi

if [[ -f "${EBPF_LOCKFILE}" ]]; then
  cargo metadata --format-version 1 --manifest-path "${EBPF_MANIFEST}" >/dev/null
fi

printf '%s\n' "${TARGET_VERSION}"
