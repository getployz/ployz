#!/usr/bin/env bash
set -euo pipefail
repo_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_dir"
cargo build -p ployz-sdk --release --locked
case "$(uname -s)" in
  Darwin) binding=${CARGO_TARGET_DIR:-target}/release/libployz_sdk.dylib ;;
  Linux) binding=${CARGO_TARGET_DIR:-target}/release/libployz_sdk.so ;;
  *) echo 'Cloud SDK build requires Linux or macOS' >&2; exit 1 ;;
esac
# Replace, don't overwrite: macOS caches the code signature per inode and SIGKILLs a stale one.
rm -f crates/ployz-sdk/ployz-sdk.node && cp "$binding" crates/ployz-sdk/ployz-sdk.node
bash scripts/build-config-browser.sh
node crates/ployz-sdk/tests/config-contract.mjs
node --test crates/ployz-sdk/tests/node_logs.js
