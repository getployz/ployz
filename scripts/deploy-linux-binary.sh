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

REMOTE_SHA_CMD='if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi'

# --- helpers ---

local_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
    return
  fi
  shasum -a 256 "$1" | awk '{print $1}'
}

# Upload a file to the remote target if its checksum differs.
# Touches $RESTART_FLAG if any file was uploaded.
# Usage: upload_file <target> <local_path> <remote_path> <mode>
upload_file() {
  local target=$1 local_path=$2 remote_path=$3 mode=$4
  local name tmp_path local_sha remote_sha
  name="$(basename "$remote_path")"
  tmp_path="/tmp/${name}-$$"
  local_sha="$(local_sha256 "$local_path")"

  remote_sha="$(ssh -p "$SSH_PORT" "$target" \
    "if [ -f '$remote_path' ]; then sudo ${REMOTE_SHA_CMD} '$remote_path' | awk '{print \$1}'; fi" \
    2>/dev/null || true)"

  if [[ -n "$remote_sha" && "$remote_sha" == "$local_sha" ]]; then
    echo "   $name unchanged, skipping"
    return
  fi

  echo "   uploading $name"
  if command -v rsync >/dev/null 2>&1; then
    rsync -az -e "ssh -p $SSH_PORT" "$local_path" "$target:$tmp_path"
  else
    scp -C -P "$SSH_PORT" "$local_path" "$target:$tmp_path"
  fi
  ssh -p "$SSH_PORT" "$target" "sudo install -m $mode '$tmp_path' '$remote_path' && rm -f '$tmp_path'"
  touch "$RESTART_FLAG"
}

# --- targets ---

if [[ $# -gt 0 ]]; then
  TARGETS_RAW="$*"
fi

if [[ -z "$TARGETS_RAW" ]]; then
  echo "No targets set. Add TARGETS to $ENV_FILE or pass hosts as args." >&2
  exit 1
fi

read -r -a TARGET_LIST <<<"$TARGETS_RAW"

# --- build ---

echo "==> Compiling Linux daemon binary"

"$ROOT_DIR/scripts/install-ebpf-bytecode.sh"

if command -v cross >/dev/null 2>&1; then
  cross build --release --target x86_64-unknown-linux-gnu -p ployzd --features ebpf-native
  cross build --release --target x86_64-unknown-linux-gnu -p ployz-gateway -p ployz-dns
elif command -v cargo-zigbuild >/dev/null 2>&1; then
  cargo zigbuild --release --target x86_64-unknown-linux-gnu -p ployzd --features ebpf-native
  cargo zigbuild --release --target x86_64-unknown-linux-gnu -p ployz-gateway -p ployz-dns
else
  echo "   No cross-compiler found. Trying cargo build --target directly..."
  echo "   (install 'cross' or 'cargo-zigbuild' for reliable cross-compilation)"
  cargo build --release --target x86_64-unknown-linux-gnu -p ployzd --features ebpf-native
  cargo build --release --target x86_64-unknown-linux-gnu -p ployz-gateway -p ployz-dns
fi

TARGET_DIR="$ROOT_DIR/target/x86_64-unknown-linux-gnu/release"

# --- deploy ---

FILES=(
  "$TARGET_DIR/ployzd:/usr/local/bin/ployzd:0755"
  "$TARGET_DIR/ployz-gateway:/usr/local/bin/ployz-gateway:0755"
  "$TARGET_DIR/ployz-dns:/usr/local/bin/ployz-dns:0755"
  "$ROOT_DIR/packaging/bin/ployz:/usr/local/bin/ployz:0755"
  "$ROOT_DIR/packaging/systemd/ployzd.service:/etc/systemd/system/ployzd.service:0644"
)

RESTART_FLAG=$(mktemp)
rm -f "$RESTART_FLAG"
trap 'rm -f "$RESTART_FLAG"' EXIT

for target in "${TARGET_LIST[@]}"; do
  [[ -z "$target" ]] && continue
  echo "==> Deploying to $target"
  rm -f "$RESTART_FLAG"
  PIDS=()

  for entry in "${FILES[@]}"; do
    IFS=: read -r local_path remote_path mode <<<"$entry"
    upload_file "$target" "$local_path" "$remote_path" "$mode" &
    PIDS+=($!)
  done

  FAILED=0
  for pid in "${PIDS[@]}"; do
    wait "$pid" || FAILED=1
  done
  if [[ "$FAILED" == "1" ]]; then
    echo "   ERROR: one or more uploads failed for $target" >&2
    continue
  fi
  if [[ -f "$RESTART_FLAG" ]]; then
    echo "   restarting ployzd"
    ssh -p "$SSH_PORT" "$target" "sudo systemctl daemon-reload || true; sudo systemctl restart ployzd.service"
  fi
done

echo "Deploy complete"
