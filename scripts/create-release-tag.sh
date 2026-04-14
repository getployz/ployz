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
  # Detect release-branch merges — these always proceed to tag logic even if
  # Cargo.toml is unchanged, so a failed release can be re-triggered by
  # re-merging the release PR.
  # Only match the canonical GitHub merge-commit format where the head branch
  # itself starts with "release/" (e.g. release/next), not branches that merely
  # contain "release/" in a deeper path segment like feature/release-fix.
  merge_subject="$(git log -1 --format='%s' "${CURRENT_SHA}" 2>/dev/null || true)"
  is_release_merge=0
  if [[ "${merge_subject}" =~ ^Merge\ pull\ request\ \#[0-9]+\ from\ [^/]+/release/ ]]; then
    is_release_merge=1
  fi

  if [[ "${is_release_merge}" == "0" ]]; then
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
fi

current_commit="$(git rev-parse "${CURRENT_SHA}")"

# Resolve existing tag target (if any) from local or remote
existing_tag_target=""
if git rev-parse --verify "refs/tags/${current_tag}" >/dev/null 2>&1; then
  existing_tag_target="$(git rev-list -1 "${current_tag}")"
elif git ls-remote --exit-code --tags origin "refs/tags/${current_tag}" >/dev/null 2>&1; then
  git fetch origin "refs/tags/${current_tag}:refs/tags/${current_tag}" 2>/dev/null || true
  if git rev-parse --verify "refs/tags/${current_tag}" >/dev/null 2>&1; then
    existing_tag_target="$(git rev-list -1 "${current_tag}")"
  fi
fi

if [[ -n "${existing_tag_target}" ]]; then
  if [[ "${existing_tag_target}" == "${current_commit}" ]]; then
    echo "Tag ${current_tag} already exists at ${current_commit}; skipping tag creation"
    exit 0
  fi
  echo "Tag ${current_tag} exists at ${existing_tag_target} but current commit is ${current_commit}; re-tagging"
  git tag -d "${current_tag}" 2>/dev/null || true
  git push origin ":refs/tags/${current_tag}" 2>/dev/null || true
fi

git tag -a "${current_tag}" -m "release: ${current_tag}" "${CURRENT_SHA}"
git push origin "${current_tag}"
