#!/usr/bin/env bash
set -euo pipefail
repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
node_out=crates/ployz-sdk/ployz-sdk.node
# `eval` lets the addon dial unix sockets, for the agent eval's fake fleet. Images never set it.
features=${PLOYZ_SDK_FEATURES:-}

# Worktrees share built bindings through scripts/artifacts.py, keyed by the sources they come from, edits included.
# kache can't cover this: our crates' keys carry the checkout's absolute paths, so every new worktree recompiles them.
# Without git or python3 (the Docker image build) there is no key, so it always builds.
key=
if command -v python3 >/dev/null && git rev-parse --git-dir >/dev/null 2>&1; then
  key=$(python3 scripts/artifacts.py key sdk --extra "features=$features")
fi

# SDK=stale lets an edit that leaves the SDK's interface alone run on main's build, stale but quick.
stale=
if [ "${SDK:-}" = stale ]; then
  stale="SDK=exact to build"
fi

if [ -z "$key" ] || ! python3 scripts/artifacts.py fetch sdk --extra "features=$features" ${stale:+--stale-ok "$stale"} crates/ployz-sdk; then
  cargo build -p ployz-sdk --release --locked ${features:+--features "$features"}
  case "$(uname -s)" in
    Darwin) binding=${CARGO_TARGET_DIR:-target}/release/libployz_sdk.dylib ;;
    Linux) binding=${CARGO_TARGET_DIR:-target}/release/libployz_sdk.so ;;
    *) echo 'Cloud SDK build requires Linux or macOS' >&2; exit 1 ;;
  esac
  # Replace, don't overwrite: macOS caches the code signature per inode and SIGKILLs a stale one.
  rm -f "$node_out" && cp "$binding" "$node_out"
  bash scripts/build-config-wasm.sh
  if [ -n "$key" ]; then
    python3 scripts/artifacts.py store sdk --extra "features=$features" --expect "$key" crates/ployz-sdk
  fi
fi
node crates/ployz-sdk/tests/config-contract.mjs
node --test crates/ployz-sdk/tests/node_logs.js
