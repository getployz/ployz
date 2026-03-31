#!/usr/bin/env bash
set -euo pipefail

REPO="${GITHUB_REPOSITORY:-}"
TAG_NAME=""
TARGET_COMMITISH=""
PREVIOUS_TAG=""
MODE="release"

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

generated_body="$(gh api "${api_args[@]}" --jq '.body')"

if [[ -z "${generated_body}" ]]; then
  generated_body="No generated notes were returned."
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

- Edit \`Cargo.toml\`, \`Cargo.lock\`, \`ebpf/Cargo.toml\`, and \`ebpf/Cargo.lock\` in this PR if you want a non-patch release.
- Merging this PR on \`main\` will create tag \`${TAG_NAME}\` automatically and trigger the publish workflow.
EOF
    ;;
  *)
    printf 'unknown mode: %s\n' "${MODE}" >&2
    exit 1
    ;;
esac
