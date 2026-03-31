#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CURRENT_SHA="${GITHUB_SHA:-HEAD}"
BEFORE_SHA="${GITHUB_EVENT_BEFORE:-}"
ALL_ZERO_SHA='0000000000000000000000000000000000000000'

if [[ -z "${BEFORE_SHA}" && -n "${GITHUB_EVENT_PATH:-}" && -f "${GITHUB_EVENT_PATH}" ]]; then
  BEFORE_SHA="$(jq -r '.before // empty' "${GITHUB_EVENT_PATH}")"
fi

current_version="$(bash "${ROOT_DIR}/scripts/read-workspace-version.sh")"
current_tag="v${current_version}"

if [[ -n "${BEFORE_SHA}" && "${BEFORE_SHA}" != "${ALL_ZERO_SHA}" ]]; then
  if git diff --quiet "${BEFORE_SHA}" "${CURRENT_SHA}" -- Cargo.toml; then
    echo "Cargo.toml did not change in this push; skipping tag creation"
    exit 0
  fi

  before_manifest="$(mktemp)"
  git show "${BEFORE_SHA}:Cargo.toml" > "${before_manifest}"
  before_version="$(bash "${ROOT_DIR}/scripts/read-workspace-version.sh" --manifest "${before_manifest}")"
  rm -f "${before_manifest}"

  if [[ "${before_version}" == "${current_version}" ]]; then
    echo "Workspace version did not change in this push; skipping tag creation"
    exit 0
  fi
fi

if git rev-parse --verify "${current_tag}" >/dev/null 2>&1; then
  echo "Tag ${current_tag} already exists locally; skipping tag creation"
  exit 0
fi

if git ls-remote --exit-code --tags origin "refs/tags/${current_tag}" >/dev/null 2>&1; then
  echo "Tag ${current_tag} already exists on origin; skipping tag creation"
  exit 0
fi

git tag -a "${current_tag}" -m "release: ${current_tag}" "${CURRENT_SHA}"
git push origin "${current_tag}"
