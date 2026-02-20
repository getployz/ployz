#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${ENV_FILE:-$ROOT_DIR/.env.targets}"

if [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE"
fi

SSH_PORT="${SSH_PORT:-22}"
TARGETS_RAW="${TARGETS:-}"
DEST_PATH="${DEST_PATH:-/usr/local/bin/ployz}"
BIN_PATH="$ROOT_DIR/bin/ployz-linux-amd64"

local_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
    return
  fi
  shasum -a 256 "$1" | awk '{print $1}'
}

remote_sha256_cmd='if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi'

if [[ $# -gt 0 ]]; then
  TARGETS_RAW="$*"
fi

if [[ -z "$TARGETS_RAW" ]]; then
  echo "No targets set. Add TARGETS to $ENV_FILE or pass hosts as args." >&2
  exit 1
fi

read -r -a TARGET_LIST <<<"$TARGETS_RAW"

echo "==> Building Linux binary"
GOOS=linux GOARCH=amd64 go build -ldflags="-s -w" -o "$BIN_PATH" ./cmd/ployz

LOCAL_SHA="$(local_sha256 "$BIN_PATH")"

for target in "${TARGET_LIST[@]}"; do
  [[ -z "$target" ]] && continue
  echo "==> Deploying to $target"

  TMP_PATH="/tmp/ployz-$$"

  REMOTE_SHA="$(ssh -p "$SSH_PORT" "$target" "if [ -f '$DEST_PATH' ]; then sudo ${remote_sha256_cmd} '$DEST_PATH' | awk '{print \$1}'; fi" 2>/dev/null || true)"
  if [[ -n "$REMOTE_SHA" && "$REMOTE_SHA" == "$LOCAL_SHA" ]]; then
    echo "   unchanged, skipping upload"
    continue
  fi

  if command -v rsync >/dev/null 2>&1; then
    rsync -az --progress -e "ssh -p $SSH_PORT" "$BIN_PATH" "$target:$TMP_PATH"
  else
    scp -C -P "$SSH_PORT" "$BIN_PATH" "$target:$TMP_PATH"
  fi
  ssh -p "$SSH_PORT" "$target" "sudo install -m 0755 '$TMP_PATH' '$DEST_PATH' && rm -f '$TMP_PATH' && '$DEST_PATH' --help >/dev/null"
done

echo "Deploy complete"
