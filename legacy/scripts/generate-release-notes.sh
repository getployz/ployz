#!/usr/bin/env bash
set -euo pipefail

REPO="${GITHUB_REPOSITORY:-}"
TAG_NAME=""
TARGET_COMMITISH=""
PREVIOUS_TAG=""
MODE="release"

generate_notes_body() {
  local api_args

  api_args=(
    "repos/${REPO}/releases/generate-notes"
    -X POST
    -f "tag_name=${TAG_NAME}"
  )

  if [[ -n "${TARGET_COMMITISH}" ]]; then
    api_args+=(-f "target_commitish=${TARGET_COMMITISH}")
  fi

  if [[ -n "${PREVIOUS_TAG}" ]]; then
    api_args+=(-f "previous_tag_name=${PREVIOUS_TAG}")
  fi

  gh api "${api_args[@]}" --jq '.body'
}

notes_include_changes() {
  local body=$1

  [[ "${body}" == *"## What's Changed"* ]]
}

manual_release_notes() {
  local compare_head compare_head_api compare_commits pr_rows

  if [[ -z "${PREVIOUS_TAG}" ]]; then
    return 1
  fi

  compare_head="${TARGET_COMMITISH:-${TAG_NAME}}"
  compare_head_api="${compare_head//\//%2F}"
  compare_commits="$(
    gh api "repos/${REPO}/compare/${PREVIOUS_TAG}...${compare_head_api}" \
      --jq '.commits.[].sha' 2>/dev/null || true
  )"

  if [[ -z "${compare_commits}" ]]; then
    return 1
  fi

  pr_rows="$(
    while IFS= read -r sha; do
      [[ -n "${sha}" ]] || continue
      gh api \
        -H "Accept: application/vnd.github+json" \
        "repos/${REPO}/commits/${sha}/pulls" \
        --jq '.[] | [.number, .title, .user.login, .html_url] | @tsv' 2>/dev/null || true
    done <<< "${compare_commits}"
  )"

  if [[ -z "${pr_rows}" ]]; then
    return 1
  fi

  {
    printf "## What's Changed\n"
    printf '%s\n' "${pr_rows}" \
      | sort -t "$(printf '\t')" -k1,1n \
      | awk -F '\t' '!seen[$1]++ { printf "* %s by @%s in %s\n", $2, $3, $4 }'
    printf '\n\n**Full Changelog**: https://github.com/%s/compare/%s...%s\n' \
      "${REPO}" "${PREVIOUS_TAG}" "${compare_head}"
  }
}

usage() {
  cat <<'EOF'
Usage:
  scripts/generate-release-notes.sh --tag TAG [--target-commitish REF] [--previous-tag TAG] [--repo OWNER/REPO] [--mode release|pr]
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      TAG_NAME=${2:-}
      shift 2
      ;;
    --target-commitish)
      TARGET_COMMITISH=${2:-}
      shift 2
      ;;
    --previous-tag)
      PREVIOUS_TAG=${2:-}
      shift 2
      ;;
    --repo)
      REPO=${2:-}
      shift 2
      ;;
    --mode)
      MODE=${2:-}
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

if [[ -z "${REPO}" || -z "${TAG_NAME}" ]]; then
  usage >&2
  exit 1
fi

generated_body=""
for attempt in 1 2 3; do
  generated_body="$(generate_notes_body)"
  if notes_include_changes "${generated_body}"; then
    break
  fi
  if [[ -z "${PREVIOUS_TAG}" || "${attempt}" == "3" ]]; then
    break
  fi
  sleep 5
done

if [[ -z "${generated_body}" ]]; then
  generated_body="No generated notes were returned."
fi

if ! notes_include_changes "${generated_body}"; then
  manual_body="$(manual_release_notes || true)"
  if [[ -n "${manual_body}" ]]; then
    generated_body="${manual_body}"
  fi
fi

case "${MODE}" in
  release)
    printf '%s\n' "${generated_body}"
    ;;
  pr)
    cat <<EOF
## Summary

Prepare \`${TAG_NAME}\` for release from \`main\`.

## Generated Notes

${generated_body}

## Maintainer Notes

- Comment \`/release set X.Y.Z\` or \`/release patch|minor|major\` on this PR to change the release version.
- Direct edits to \`Cargo.toml\`, \`Cargo.lock\`, \`ebpf/Cargo.toml\`, and \`ebpf/Cargo.lock\` on this PR still work.
- Merging this PR on \`main\` will create tag \`${TAG_NAME}\` automatically and trigger the publish workflow.
EOF
    ;;
  *)
    printf 'unknown mode: %s\n' "${MODE}" >&2
    exit 1
    ;;
esac
