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
cp "$binding" crates/ployz-sdk/ployz-sdk.node
CGO_ENABLED=0 PLOYZ_VERSION=$(node -p 'require("./crates/ployz-sdk/package.json").version') bash native/tailcat/build.sh "$repo_dir/crates/ployz-sdk/ployz-tailcat"
bash scripts/build-config-browser.sh
node crates/ployz-sdk/tests/config-contract.mjs
