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

if [[ $# -gt 0 ]]; then
  TARGETS_RAW="$*"
fi

if [[ -z "$TARGETS_RAW" ]]; then
  echo "No targets set. Add TARGETS to $ENV_FILE or pass hosts as args." >&2
  exit 1
fi

read -r -a TARGET_LIST <<<"$TARGETS_RAW"

PAYLOAD_DIR="$(mktemp -d)"
trap 'rm -rf "$PAYLOAD_DIR"' EXIT

bash "$ROOT_DIR/scripts/build-install-payload.sh" --output "$PAYLOAD_DIR"

for target in "${TARGET_LIST[@]}"; do
  [[ -z "$target" ]] && continue
  echo "==> Deploying to $target"
  ssh -p "$SSH_PORT" "$target" "rm -rf /tmp/ployz-payload && mkdir -p /tmp/ployz-payload"
  if command -v rsync >/dev/null 2>&1; then
    rsync -az -e "ssh -p $SSH_PORT" "$PAYLOAD_DIR"/ "$target:/tmp/ployz-payload/"
  else
    scp -Cr -P "$SSH_PORT" "$PAYLOAD_DIR"/. "$target:/tmp/ployz-payload/"
  fi
  scp -C -P "$SSH_PORT" "$ROOT_DIR/ployz.sh" "$target:/tmp/ployz.sh"
  ssh -p "$SSH_PORT" "$target" "bash /tmp/ployz.sh install --source payload --payload-dir /tmp/ployz-payload --mode host-service"
done

echo "Deploy complete"
