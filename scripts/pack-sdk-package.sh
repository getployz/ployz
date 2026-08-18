#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
PACKAGE_ROOT=${PLOYZ_SDK_PACKAGE_ROOT:-"$ROOT/ployz-sdk"}

fail() { echo "sdk pack failed: $1" >&2; exit 1; }

dest=${1:-}
cdylib=${2:-}
[ -n "$dest" ] || fail "usage: $0 <dest-dir> <cdylib>"
[ -n "$cdylib" ] || fail "native binding is required"
[ -f "$cdylib" ] || fail "native binding '$cdylib' is missing"
[ -f "$PACKAGE_ROOT/package.json" ] || fail "package.json is missing"

if grep -Eq '"private":[[:space:]]*true' "$PACKAGE_ROOT/package.json"; then
    fail "refusing to pack a private package"
fi

mkdir -p "$dest"
cp "$PACKAGE_ROOT/package.json" "$PACKAGE_ROOT/index.js" "$PACKAGE_ROOT/index.d.ts" "$dest/"
cp -a "$PACKAGE_ROOT/generated" "$dest/generated"
cp "$cdylib" "$dest/ployz-sdk.node"

if ! grep -Eq '"access":[[:space:]]*"public"' "$dest/package.json"; then
    fail "packed package.json is missing public publishConfig"
fi
[ -s "$dest/ployz-sdk.node" ] || fail "packed native binding is empty"
