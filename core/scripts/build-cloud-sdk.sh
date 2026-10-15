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
# CI builds release and a local build dev, each cached under its own profile; a local run takes either.
release="features=$features profile=release"
extra=$release
if [ -z "${CI:-}" ]; then
  extra="features=$features profile=dev"
fi
key=
if command -v python3 >/dev/null && git rev-parse --git-dir >/dev/null 2>&1; then
  key=$(python3 scripts/artifacts.py key sdk --extra "$extra")
fi

# SDK=stale lets an edit that leaves the SDK's interface alone run on main's build, stale but quick.
stale=
if [ "${SDK:-}" = stale ]; then
  stale="SDK=exact to build"
fi

fetch() { python3 scripts/artifacts.py fetch sdk "$@" crates/ployz-sdk; }
if [ -z "$key" ] || ! { fetch --extra "$release" || { [ "$extra" != "$release" ] && fetch --extra "$extra"; } ||
  { [ -n "$stale" ] && fetch --extra "$release" --stale-ok "$stale"; }; }; then
  # napi-derive writes each #[napi] item's TS declaration here; a fresh path makes napi-build re-emit them all.
  TYPE_DEF_TMP_PATH=$(mktemp)
  export TYPE_DEF_TMP_PATH
  trap 'rm -f "$TYPE_DEF_TMP_PATH"' EXIT
  # CI and images ship this SDK, so they build release. A local build only serves the dev loop, so it
  # skips release's optimizations.
  if [ -n "$key" ] && [ -z "${CI:-}" ]; then
    python3 scripts/artifacts.py build cargo build -p ployz-sdk --locked ${features:+--features "$features"}
    profile_dir=debug
  else
    cargo build -p ployz-sdk --release --locked ${features:+--features "$features"}
    profile_dir=release
  fi
  case "$(uname -s)" in
    Darwin) binding=${CARGO_TARGET_DIR:-target}/$profile_dir/libployz_sdk.dylib ;;
    Linux) binding=${CARGO_TARGET_DIR:-target}/$profile_dir/libployz_sdk.so ;;
    *) echo 'Cloud SDK build requires Linux or macOS' >&2; exit 1 ;;
  esac
  # Replace, don't overwrite: macOS caches the code signature per inode and SIGKILLs a stale one.
  rm -f "$node_out" && cp "$binding" "$node_out"
  "${CARGO_TARGET_DIR:-target}/$profile_dir/sdk-generated" crates/ployz-sdk/generated
  node crates/ployz-sdk/scripts/native-dts.mjs "$TYPE_DEF_TMP_PATH" crates/ployz-sdk/generated/native.d.ts
  # The config WASM depends on fewer crates than the addon, so an addon edit often leaves it current.
  wasm_key=
  if [ -n "$key" ]; then
    wasm_key=$(python3 scripts/artifacts.py key config-wasm)
  fi
  if [ -z "$wasm_key" ]; then
    bash scripts/build-config-wasm.sh
  elif ! python3 scripts/artifacts.py fetch config-wasm crates/ployz-sdk; then
    python3 scripts/artifacts.py build bash scripts/build-config-wasm.sh
    python3 scripts/artifacts.py store config-wasm --expect "$wasm_key" crates/ployz-sdk
  fi
  if [ -n "$key" ]; then
    python3 scripts/artifacts.py store sdk --extra "$extra" --expect "$key" crates/ployz-sdk
  fi
fi
node crates/ployz-sdk/tests/config-contract.mjs
node --test crates/ployz-sdk/tests/node_logs.js
