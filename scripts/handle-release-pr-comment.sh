#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${GITHUB_REPOSITORY:-}"
EVENT_PATH="${GITHUB_EVENT_PATH:-}"
RELEASE_BRANCH="release/next"
BASE_BRANCH="main"

usage_comment() {
  cat <<'EOF'
Release command failed.

Supported commands:
- `/release set X.Y.Z`
- `/release patch`
- `/release minor`
- `/release major`
EOF
}

post_status_comment() {
  local pr_number=$1
  local body=$2

  gh pr comment "${pr_number}" --repo "${REPO}" --body "${body}" >/dev/null
}

reply_and_exit() {
  local pr_number=$1
  local body=$2
  local code=${3:-0}

  post_status_comment "${pr_number}" "${body}"
  exit "${code}"
}

require_event_context() {
  if [[ -z "${REPO}" || -z "${EVENT_PATH}" || ! -f "${EVENT_PATH}" ]]; then
    printf 'GITHUB_REPOSITORY and GITHUB_EVENT_PATH are required\n' >&2
    exit 1
  fi
}

current_release_version() {
  bash "${ROOT_DIR}/scripts/read-workspace-version.sh"
}

remote_release_version() {
  local manifest_path

  git fetch --force origin "${RELEASE_BRANCH}" >/dev/null 2>&1
  manifest_path="$(mktemp)"
  git show "origin/${RELEASE_BRANCH}:Cargo.toml" > "${manifest_path}"
  bash "${ROOT_DIR}/scripts/read-workspace-version.sh" --manifest "${manifest_path}"
  rm -f "${manifest_path}"
}

require_event_context

if ! jq -e '.issue.pull_request' "${EVENT_PATH}" >/dev/null 2>&1; then
  exit 0
fi

comment_body="$(jq -r '.comment.body // empty' "${EVENT_PATH}")"
pr_number="$(jq -r '.issue.number // empty' "${EVENT_PATH}")"
commenter="$(jq -r '.comment.user.login // empty' "${EVENT_PATH}")"
author_association="$(jq -r '.comment.author_association // "NONE"' "${EVENT_PATH}")"

if [[ "${commenter}" == "github-actions[bot]" ]]; then
  exit 0
fi

if [[ "${comment_body}" != /release* ]]; then
  exit 0
fi

pr_json="$(gh api "repos/${REPO}/pulls/${pr_number}")"
pr_state="$(jq -r '.state' <<< "${pr_json}")"
pr_head_ref="$(jq -r '.head.ref' <<< "${pr_json}")"
pr_base_ref="$(jq -r '.base.ref' <<< "${pr_json}")"

if [[ "${pr_state}" != "open" || "${pr_head_ref}" != "${RELEASE_BRANCH}" || "${pr_base_ref}" != "${BASE_BRANCH}" ]]; then
  exit 0
fi

case "${author_association}" in
  OWNER|COLLABORATOR)
    ;;
  *)
  reply_and_exit \
    "${pr_number}" \
    "Permission denied. Release commands require owner or collaborator access." \
    0
    ;;
esac

manage_args=(
  --repo "${REPO}"
  --pr-number "${pr_number}"
)

if [[ "${comment_body}" =~ ^/release[[:space:]]+set[[:space:]]+([0-9]+\.[0-9]+\.[0-9]+)[[:space:]]*$ ]]; then
  requested_version="${BASH_REMATCH[1]}"
  before_version="$(remote_release_version)"
  manage_args+=(--set-version "${requested_version}")
elif [[ "${comment_body}" =~ ^/release[[:space:]]+(patch|minor|major)[[:space:]]*$ ]]; then
  requested_bump="${BASH_REMATCH[1]}"
  before_version="$(remote_release_version)"
  manage_args+=(--bump-version "${requested_bump}")
else
  reply_and_exit "${pr_number}" "$(usage_comment)" 0
fi

if ! output="$(bash "${ROOT_DIR}/scripts/manage-release-pr.sh" "${manage_args[@]}" 2>&1)"; then
  error_message="$(printf '%s\n' "${output}" | head -n 1)"
  if [[ -z "${error_message}" ]]; then
    error_message="Unknown error."
  fi
  reply_and_exit "${pr_number}" "Release command failed: ${error_message}" 1
fi

after_version="$(current_release_version)"

if [[ "${before_version}" == "${after_version}" ]]; then
  reply_and_exit "${pr_number}" "Release PR already targets v${after_version}." 0
fi

reply_and_exit "${pr_number}" "Updated release PR to v${after_version}." 0
