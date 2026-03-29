#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="raw"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      MODE="tag"
      shift
      ;;
    --help|-h)
      printf 'Usage: scripts/read-workspace-version.sh [--tag]\n' >&2
      exit 0
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

version="$(
  awk '
    /^\[workspace\.package\]$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^version = "/ {
      line = $0
      sub(/^version = "/, "", line)
      sub(/"$/, "", line)
      print line
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "${ROOT_DIR}/Cargo.toml"
)"

case "${MODE}" in
  raw)
    printf '%s\n' "${version}"
    ;;
  tag)
    printf 'v%s\n' "${version}"
    ;;
esac
