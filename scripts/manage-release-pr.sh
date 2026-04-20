#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_BRANCH="main"
RELEASE_BRANCH="release/next"
REPO="${GITHUB_REPOSITORY:-}"
DEFAULT_BUMP="patch"
EXISTING_RELEASE_REF=""
SET_VERSION=""
BUMP_VERSION=""
PR_NUMBER=""
RELEASE_FILES=(
  "Cargo.toml"
  "Cargo.lock"
  "ebpf/Cargo.toml"
  "ebpf/Cargo.lock"
)

usage() {
  cat <<'EOF'
Usage:
  scripts/manage-release-pr.sh [--base BRANCH] [--branch BRANCH] [--repo OWNER/REPO] [--default-bump patch|minor|major] [--pr-number NUMBER] [--set-version X.Y.Z | --bump-version patch|minor|major]
EOF
}

latest_release_tag() {
  gh release list \
    --repo "${REPO}" \
    --exclude-drafts --exclude-pre-releases --limit 100 \
    --json tagName \
    --jq '[.[] | select(.tagName | test("^v")) | .tagName][0] // empty'
}

release_version_from_tag() {
  local tag=$1

  if [[ -z "${tag}" ]]; then
    printf '\n'
    return 0
  fi

  if [[ ! "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    printf 'expected release tag vX.Y.Z, got %s\n' "${tag}" >&2
    exit 1
  fi

  printf '%s\n' "${BASH_REMATCH[1]}"
}

bump_semver() {
  local version=$1
  local level=$2
  local major minor patch

  if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'expected semantic version X.Y.Z, got %s\n' "${version}" >&2
    exit 1
  fi

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

read_version_from_ref() {
  local ref=$1
  local manifest_path

  manifest_path="$(mktemp)"
  git show "${ref}:Cargo.toml" > "${manifest_path}"
  bash "${ROOT_DIR}/scripts/read-workspace-version.sh" --manifest "${manifest_path}"
  rm -f "${manifest_path}"
}

current_workspace_version() {
  bash "${ROOT_DIR}/scripts/read-workspace-version.sh"
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
    --pr-number)
      PR_NUMBER=${2:-}
      shift 2
      ;;
    --default-bump)
      DEFAULT_BUMP=${2:-}
      shift 2
      ;;
    --set-version)
      SET_VERSION=${2:-}
      shift 2
      ;;
    --bump-version)
      BUMP_VERSION=${2:-}
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

if [[ -n "${SET_VERSION}" && -n "${BUMP_VERSION}" ]]; then
  printf 'specify either --set-version or --bump-version, not both\n' >&2
  exit 1
fi

git fetch --force --tags origin "${BASE_BRANCH}"

previous_tag="$(latest_release_tag)"
main_ref="origin/${BASE_BRANCH}"
main_version="$(read_version_from_ref "${main_ref}")"
latest_release_version="$(release_version_from_tag "${previous_tag}")"
release_pr_number="${PR_NUMBER}"
target_version=""
preserve_release_files=0
existing_release_sha=""
changed=0

if [[ -z "${release_pr_number}" ]]; then
  release_pr_number="$(open_release_pr_number)"
fi

if [[ -n "${SET_VERSION}" || -n "${BUMP_VERSION}" ]]; then
  if ! remote_release_branch_exists; then
    printf 'release branch %s does not exist on origin\n' "${RELEASE_BRANCH}" >&2
    exit 1
  fi

  git fetch --force origin "${RELEASE_BRANCH}"
  EXISTING_RELEASE_REF="origin/${RELEASE_BRANCH}"
  existing_release_sha="$(git rev-parse "${EXISTING_RELEASE_REF}")"
  git checkout -B "${RELEASE_BRANCH}" "${main_ref}"

  if [[ -n "${SET_VERSION}" ]]; then
    target_version="${SET_VERSION}"
  else
    bump_base_version="${latest_release_version}"
    if [[ -z "${bump_base_version}" ]]; then
      bump_base_version="${main_version}"
    fi
    target_version="$(bump_semver "${bump_base_version}" "${BUMP_VERSION}")"
  fi

  current_release_version="$(current_workspace_version)"
  if [[ "${current_release_version}" != "${target_version}" ]]; then
    bash "${ROOT_DIR}/scripts/update-workspace-version.sh" \
      --root "${ROOT_DIR}" \
      --set "${target_version}" >/dev/null
  fi
elif remote_release_branch_exists; then
  git fetch --force origin "${RELEASE_BRANCH}"
  EXISTING_RELEASE_REF="origin/${RELEASE_BRANCH}"
  existing_release_sha="$(git rev-parse "${EXISTING_RELEASE_REF}")"
  if [[ -n "${release_pr_number}" ]]; then
    if ! git diff --quiet "${main_ref}" "${EXISTING_RELEASE_REF}" -- "${RELEASE_FILES[@]}"; then
      preserve_release_files=1
      target_version="$(read_version_from_ref "${EXISTING_RELEASE_REF}")"
    else
      release_version="$(read_version_from_ref "${EXISTING_RELEASE_REF}")"
      if [[ "${release_version}" != "${main_version}" ]]; then
        target_version="${release_version}"
      fi
    fi
  else
    target_version="$(
      bash "${ROOT_DIR}/scripts/update-workspace-version.sh" \
        --root "${ROOT_DIR}" \
        --bump "${DEFAULT_BUMP}"
    )"
  fi

  git checkout -B "${RELEASE_BRANCH}" "${main_ref}"

  if [[ -n "${release_pr_number}" && "${preserve_release_files}" == "1" ]]; then
    git checkout "${EXISTING_RELEASE_REF}" -- "${RELEASE_FILES[@]}"
  elif [[ -n "${release_pr_number}" && -n "${target_version}" && "${target_version}" != "${main_version}" ]]; then
    bash "${ROOT_DIR}/scripts/update-workspace-version.sh" \
      --root "${ROOT_DIR}" \
      --set "${target_version}" >/dev/null
  elif [[ -z "${release_pr_number}" ]]; then
    bash "${ROOT_DIR}/scripts/update-workspace-version.sh" \
      --root "${ROOT_DIR}" \
      --set "${target_version}" >/dev/null
  else
    target_version="$(
      bash "${ROOT_DIR}/scripts/update-workspace-version.sh" \
        --root "${ROOT_DIR}" \
        --bump "${DEFAULT_BUMP}"
    )"
  fi
else
  git checkout -B "${RELEASE_BRANCH}" "${main_ref}"
  target_version="$(
    bash "${ROOT_DIR}/scripts/update-workspace-version.sh" \
      --root "${ROOT_DIR}" \
      --bump "${DEFAULT_BUMP}"
  )"
fi

if [[ -z "${target_version}" ]]; then
  target_version="$(current_workspace_version)"
fi

if ! git diff --quiet -- "${RELEASE_FILES[@]}"; then
  git add "${RELEASE_FILES[@]}"
  git commit -m "release: v${target_version}"
fi

final_release_sha="$(git rev-parse HEAD)"
if [[ -z "${existing_release_sha}" || "${existing_release_sha}" != "${final_release_sha}" ]]; then
  git push --force-with-lease origin "${RELEASE_BRANCH}"
  changed=1
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

printf 'version=%s\n' "${target_version}"
if [[ "${changed}" == "1" ]]; then
  printf 'changed=true\n'
else
  printf 'changed=false\n'
fi
