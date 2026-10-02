#!/usr/bin/env bash
set -euo pipefail
repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
node_out=crates/ployz-sdk/ployz-sdk.node

# Worktrees share built bindings, keyed by the committed sources they come from (as cloud.yml's CI cache is).
# Uncommitted changes to those sources, or no git (Docker), always build. kache can't cover this: our crates'
# keys carry the checkout's absolute paths, so every new worktree recompiles them.
inputs=(crates Cargo.toml Cargo.lock .cargo scripts/build-cloud-sdk.sh scripts/build-config-wasm.sh)
cached=
if git rev-parse --git-dir >/dev/null 2>&1 && [ -z "$(git status --porcelain -- "${inputs[@]}")" ]; then
  key=$( { git ls-tree HEAD -- "${inputs[@]}"; uname -sm; rustc -V; } | sha256sum | cut -c1-32)
  cached=${XDG_CACHE_HOME:-$HOME/.cache}/ployz/sdk/$key.node
fi

# Replace, don't overwrite: macOS caches the code signature per inode and SIGKILLs a stale one.
if [ -n "$cached" ] && [ -f "$cached" ]; then
  rm -f "$node_out" && cp "$cached" "$node_out"
  echo "build-cloud-sdk: reused $cached"
else
  cargo build -p ployz-sdk --release --locked
  case "$(uname -s)" in
    Darwin) binding=${CARGO_TARGET_DIR:-target}/release/libployz_sdk.dylib ;;
    Linux) binding=${CARGO_TARGET_DIR:-target}/release/libployz_sdk.so ;;
    *) echo 'Cloud SDK build requires Linux or macOS' >&2; exit 1 ;;
  esac
  rm -f "$node_out" && cp "$binding" "$node_out"
  bash scripts/build-config-wasm.sh
  if [ -n "$cached" ]; then
    # ponytail: never pruned (~56 MB per commit built); `rm -rf ~/.cache/ployz/sdk` when it grows.
    mkdir -p "$(dirname "$cached")" && cp "$node_out" "$cached.$$" && mv "$cached.$$" "$cached"
  fi
fi
node crates/ployz-sdk/tests/config-contract.mjs
node --test crates/ployz-sdk/tests/node_logs.js
