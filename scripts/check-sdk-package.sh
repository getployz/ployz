#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

cargo build -p ployz-sdk --locked

if [ -f target/debug/libployz_sdk.so ]; then
    cp target/debug/libployz_sdk.so ployz-sdk/ployz-sdk.node
elif [ -f target/debug/libployz_sdk.dylib ]; then
    cp target/debug/libployz_sdk.dylib ployz-sdk/ployz-sdk.node
elif [ -f target/debug/ployz_sdk.dll ]; then
    cp target/debug/ployz_sdk.dll ployz-sdk/ployz-sdk.node
else
    echo "ployz-sdk cdylib was not produced" >&2
    exit 1
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/node_modules/@ployz"
ln -s "$ROOT/ployz-sdk" "$TMP/node_modules/@ployz/sdk"

cd "$TMP"
node --input-type=commonjs <<'EOF'
const path = require("node:path");
const pkg = require("@ployz/sdk/package.json");
if (pkg.name !== "@ployz/sdk") {
  throw new Error(`expected npm name @ployz/sdk, got ${pkg.name}`);
}
const sdk = require("@ployz/sdk");
if (typeof sdk.packageName !== "function") {
  throw new Error("packageName export is missing");
}
if (sdk.packageName() !== "@ployz/sdk") {
  throw new Error(`expected packageName() to return @ployz/sdk, got ${sdk.packageName()}`);
}
if (Object.hasOwn(sdk, "connect")) {
  throw new Error("connect is out of scope for the generated-type pipeline");
}
console.log(`loaded ${path.join("@ployz", "sdk")} ${pkg.version}`);
EOF
