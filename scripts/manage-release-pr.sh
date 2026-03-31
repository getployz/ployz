#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_BRANCH="main"
RELEASE_BRANCH="release/next"
REPO="${GITHUB_REPOSITORY:-}"
DEFAULT_BUMP="patch"
EXISTING_RELEASE_REF=""
RELEASE_FILES=(
  "Cargo.toml"
  "Cargo.lock"
  "ebpf/Cargo.toml"
  "ebpf/Cargo.lock"
)

usage() {
  cat <<'EOF'
Usage:
  scripts/manage-release-pr.sh [--base BRANCH] [--branch BRANCH] [--repo OWNER/REPO] [--default-bump patch|minor|major]
EOF
}

latest_release_tag() {
  git tag --list 'v*' --sort=-version:refname | head -n 1
}

read_version_from_ref() {
  local ref=$1
  local manifest_path

  manifest_path="$(mktemp)"
  git show "${ref}:Cargo.toml" > "${manifest_path}"
  bash "${ROOT_DIR}/scripts/read-workspace-version.sh" --manifest "${manifest_path}"
  rm -f "${manifest_path}"
}

open_release_pr_number() {
  gh pr list \
    --repo "${REPO}" \
    --base "${BASE_BRANCH}" \
    --head "${RELEASE_BRANCH}" \
    --state open \
    --json number \
    --jq '.[0].number // empty'
}

remote_release_branch_exists() {
  git ls-remote --exit-code --heads origin "${RELEASE_BRANCH}" >/dev/null 2>&1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      BASE_BRANCH=${2:-}
      shift 2
      ;;
    --branch)
      RELEASE_BRANCH=${2:-}
      shift 2
      ;;
    --repo)
      REPO=${2:-}
      shift 2
      ;;
    --default-bump)
      DEFAULT_BUMP=${2:-}
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

if [[ -z "${REPO}" ]]; then
  printf 'GITHUB_REPOSITORY or --repo is required\n' >&2
  exit 1
fi

git fetch --force --tags origin "${BASE_BRANCH}"

previous_tag="$(latest_release_tag)"
main_ref="origin/${BASE_BRANCH}"
main_version="$(read_version_from_ref "${main_ref}")"
release_pr_number="$(open_release_pr_number)"
target_version=""
preserve_release_files=0

if remote_release_branch_exists; then
  git fetch --force origin "${RELEASE_BRANCH}"
  EXISTING_RELEASE_REF="origin/${RELEASE_BRANCH}"
  if ! git diff --quiet "${main_ref}" "${EXISTING_RELEASE_REF}" -- "${RELEASE_FILES[@]}"; then
    preserve_release_files=1
    target_version="$(read_version_from_ref "${EXISTING_RELEASE_REF}")"
  else
    release_version="$(read_version_from_ref "${EXISTING_RELEASE_REF}")"
    if [[ "${release_version}" != "${main_version}" ]]; then
      target_version="${release_version}"
    fi
  fi
fi

git checkout -B "${RELEASE_BRANCH}" "${main_ref}"

if [[ "${preserve_release_files}" == "1" ]]; then
  git checkout "${EXISTING_RELEASE_REF}" -- "${RELEASE_FILES[@]}"
elif [[ -n "${target_version}" ]]; then
  bash "${ROOT_DIR}/scripts/update-workspace-version.sh" --root "${ROOT_DIR}" --set "${target_version}" >/dev/null
else
  target_version="$(
    bash "${ROOT_DIR}/scripts/update-workspace-version.sh" \
      --root "${ROOT_DIR}" \
      --bump "${DEFAULT_BUMP}"
  )"
fi

if [[ -z "${target_version}" ]]; then
  target_version="$(bash "${ROOT_DIR}/scripts/read-workspace-version.sh")"
fi

if ! git diff --quiet; then
  git add "${RELEASE_FILES[@]}"
  git commit -m "release: v${target_version}"
  git push --force-with-lease origin "${RELEASE_BRANCH}"
fi

body_file="$(mktemp)"
bash "${ROOT_DIR}/scripts/generate-release-notes.sh" \
  --repo "${REPO}" \
  --tag "v${target_version}" \
  --target-commitish "${RELEASE_BRANCH}" \
  --previous-tag "${previous_tag}" \
  --mode pr > "${body_file}"

if [[ -n "${release_pr_number}" ]]; then
  gh pr edit "${release_pr_number}" \
    --repo "${REPO}" \
    --title "release: v${target_version}" \
    --body-file "${body_file}"
else
  gh pr create \
    --repo "${REPO}" \
    --base "${BASE_BRANCH}" \
    --head "${RELEASE_BRANCH}" \
    --title "release: v${target_version}" \
    --body-file "${body_file}"
fi

rm -f "${body_file}"
