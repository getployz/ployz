#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
PACKAGE_ROOT=${PLOYZ_SDK_PACKAGE_ROOT:-"$ROOT/ployz-sdk"}

fail() { echo "sdk pack failed: $1" >&2; exit 1; }

dest=${1:-}
shift || true
[ -n "$dest" ] || fail "usage: $0 <dest-dir> <binding>..."
[ "$#" -gt 0 ] || fail "native binding is required"
[ -f "$PACKAGE_ROOT/package.json" ] || fail "package.json is missing"

if grep -Eq '"private":[[:space:]]*true' "$PACKAGE_ROOT/package.json"; then
    fail "refusing to pack a private package"
fi

mkdir -p "$dest"
rm -rf "$dest/npm" # never merge into a stale pack
[ -f "$PACKAGE_ROOT/browser.mjs" ] || fail "browser.mjs is missing"
cp "$PACKAGE_ROOT/package.json" "$PACKAGE_ROOT/index.js" "$PACKAGE_ROOT/index.d.ts" "$PACKAGE_ROOT/browser.mjs" "$dest/"
cp -a "$PACKAGE_ROOT/generated" "$dest/generated"

# One package per binding: @ployz/sdk-<platform>-<arch>, injected into @ployz/sdk as an
# optional dependency so npm installs only the one matching the host. The bindings passed in
# are the source of truth, so the platform list is never declared in two places.
targets=()
for binding in "$@"; do
    [ -f "$binding" ] || fail "native binding '$binding' is missing"
    [ -s "$binding" ] || fail "native binding '$binding' is empty"
    name=$(basename "$binding")
    case "$name" in
        ployz-sdk.*-*.node) ;;
        *) fail "binding '$name' must be named ployz-sdk.<platform>-<arch>.node" ;;
    esac
    target=${name#ployz-sdk.}
    target=${target%.node}
    mkdir -p "$dest/npm/$target"
    cp "$binding" "$dest/npm/$target/ployz-sdk.node"
    targets+=("$target")
done

PLOYZ_SDK_TARGETS="${targets[*]}" node - "$dest" <<'NODE'
const fs = require("node:fs");
const [, , dest] = process.argv;
const mainPath = `${dest}/package.json`;
const main = JSON.parse(fs.readFileSync(mainPath, "utf8"));
const write = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
main.optionalDependencies = {};
for (const target of process.env.PLOYZ_SDK_TARGETS.split(" ")) {
  const [platform, arch] = target.split("-");
  const name = `${main.name}-${target}`;
  main.optionalDependencies[name] = main.version;
  write(`${dest}/npm/${target}/package.json`, {
    name,
    version: main.version,
    description: `${target} native binding for ${main.name}.`,
    repository: main.repository,
    main: "ployz-sdk.node",
    files: ["ployz-sdk.node"],
    os: [platform],
    cpu: [arch],
    engines: main.engines,
    publishConfig: main.publishConfig,
  });
}
write(mainPath, main);
NODE

if ! grep -Eq '"access":[[:space:]]*"public"' "$dest/package.json"; then
    fail "packed package.json is missing public publishConfig"
fi
