#!/usr/bin/env bash

# Sign dist/checksums.txt with the release key in MINISIGN_SECRET_KEY.
# Legacy (pure Ed25519) mode, so install.sh can verify with OpenSSL alone.

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
tag=${1:-}
[ -n "$tag" ] || { echo "usage: $0 <tag>" >&2; exit 1; }
[ -n "${MINISIGN_SECRET_KEY:-}" ] || { echo "MINISIGN_SECRET_KEY is not set" >&2; exit 1; }
dist=${DIST:-"$ROOT/dist"}
public_key=${PLOYZ_RELEASE_PUBLIC_KEY_FILE:-"$ROOT/crates/ployz-core/release.pub"}

key=$(mktemp)
trap 'rm -f "$key"' EXIT
chmod 600 "$key"
printf '%s\n' "$MINISIGN_SECRET_KEY" > "$key"

minisign -S -l -s "$key" -m "$dist/checksums.txt" -x "$dist/checksums.txt.minisig" -t "ployz $tag" < /dev/null
minisign -V -q -p "$public_key" -m "$dist/checksums.txt" -x "$dist/checksums.txt.minisig"
grep -qxF "trusted comment: ployz $tag" "$dist/checksums.txt.minisig"
