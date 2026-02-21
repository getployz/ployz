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
BIN_PLOYZD="$ROOT_DIR/bin/ployzd-linux-amd64"
BIN_RUNTIME="$ROOT_DIR/bin/ployz-runtime-linux-amd64"
DEST_PLOYZD="/usr/local/bin/ployzd"
DEST_RUNTIME="/usr/local/bin/ployz-runtime"
UNIT_PLOYZD="$ROOT_DIR/packaging/systemd/ployzd.service"
UNIT_RUNTIME="$ROOT_DIR/packaging/systemd/ployz-runtime.service"

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

echo "==> Building Linux binaries"
GOOS=linux GOARCH=amd64 go build -ldflags="-s -w" -o "$BIN_PLOYZD" ./cmd/ployzd
GOOS=linux GOARCH=amd64 go build -ldflags="-s -w" -o "$BIN_RUNTIME" ./cmd/ployz-runtime

BINS=("$BIN_PLOYZD:$DEST_PLOYZD" "$BIN_RUNTIME:$DEST_RUNTIME")
UNITS=("$UNIT_PLOYZD:/etc/systemd/system/ployzd.service" "$UNIT_RUNTIME:/etc/systemd/system/ployz-runtime.service")

for target in "${TARGET_LIST[@]}"; do
  [[ -z "$target" ]] && continue
  echo "==> Deploying to $target"
  NEEDS_RESTART=0

  for entry in "${BINS[@]}"; do
    bin_path="${entry%%:*}"
    dest_path="${entry##*:}"
    bin_name="$(basename "$dest_path")"
    local_sha="$(local_sha256 "$bin_path")"
    tmp_path="/tmp/${bin_name}-$$"

    remote_sha="$(ssh -p "$SSH_PORT" "$target" "if [ -f '$dest_path' ]; then sudo ${remote_sha256_cmd} '$dest_path' | awk '{print \$1}'; fi" 2>/dev/null || true)"
    if [[ -n "$remote_sha" && "$remote_sha" == "$local_sha" ]]; then
      echo "   $bin_name unchanged, skipping"
      continue
    fi

    echo "   uploading $bin_name"
    if command -v rsync >/dev/null 2>&1; then
      rsync -az --progress -e "ssh -p $SSH_PORT" "$bin_path" "$target:$tmp_path"
    else
      scp -C -P "$SSH_PORT" "$bin_path" "$target:$tmp_path"
    fi
    ssh -p "$SSH_PORT" "$target" "sudo install -m 0755 '$tmp_path' '$dest_path' && rm -f '$tmp_path'"
    NEEDS_RESTART=1
  done

  for entry in "${UNITS[@]}"; do
    unit_src="${entry%%:*}"
    unit_dest="${entry##*:}"
    unit_name="$(basename "$unit_dest")"
    local_sha="$(local_sha256 "$unit_src")"
    tmp_path="/tmp/${unit_name}-$$"

    remote_sha="$(ssh -p "$SSH_PORT" "$target" "if [ -f '$unit_dest' ]; then sudo ${remote_sha256_cmd} '$unit_dest' | awk '{print \$1}'; fi" 2>/dev/null || true)"
    if [[ -n "$remote_sha" && "$remote_sha" == "$local_sha" ]]; then
      echo "   $unit_name unchanged, skipping"
      continue
    fi

    echo "   uploading $unit_name"
    if command -v rsync >/dev/null 2>&1; then
      rsync -az --progress -e "ssh -p $SSH_PORT" "$unit_src" "$target:$tmp_path"
    else
      scp -C -P "$SSH_PORT" "$unit_src" "$target:$tmp_path"
    fi
    ssh -p "$SSH_PORT" "$target" "sudo install -m 0644 '$tmp_path' '$unit_dest' && rm -f '$tmp_path'"
    NEEDS_RESTART=1
  done

  if [[ "$NEEDS_RESTART" == "1" ]]; then
    echo "   restarting ployzd and ployz-runtime"
    ssh -p "$SSH_PORT" "$target" "sudo systemctl daemon-reload || true; sudo systemctl restart ployzd.service ployz-runtime.service"
  fi
done

echo "Deploy complete"
