#!/usr/bin/env bash
set -euo pipefail

log() {
  if [[ -w /dev/tty ]]; then
    printf '%s\n' "$*" > /dev/tty
    return
  fi
  printf '%s\n' "$*" >&2
}

set -- "$@"

if [[ "$(uname -s)" == "Darwin" && "${1:-}" == "run" ]]; then
  log "macOS Docker dev run detected; building local built-in images..."
  manifest_path="$(bash ./scripts/build-dev-built-in-images.sh)"
  if [[ -z "$manifest_path" ]]; then
    log "Failed to determine built-in image manifest path."
    exit 1
  fi
  export PLOYZ_BUILTIN_IMAGES_MANIFEST="$manifest_path"
  log "Using built-in image manifest: ${PLOYZ_BUILTIN_IMAGES_MANIFEST}"
fi

log "Starting ployzd: cargo run -p ployzd --bin ployzd -- $*"
exec cargo run -p ployzd --bin ployzd -- "$@"
