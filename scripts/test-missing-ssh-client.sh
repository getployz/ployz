#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/empty"

BIN=${PLOYZ_BIN:-}
if [ -z "$BIN" ]; then
    cargo build -p ployz --locked --quiet
    BIN=$ROOT/target/debug/ployz
fi
[ -x "$BIN" ] || {
    echo "missing ployz binary: $BIN" >&2
    exit 1
}

set +e
output=$(
    PATH="$TMP/empty" PLOYZ_CONFIG="$TMP/config.yaml" \
        "$BIN" machine init user@host --yes --no-dns --no-ingress --context missing-ssh 2>&1
)
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "machine init succeeded without an ssh client" >&2
    exit 1
fi
printf '%s\n' "$output" | grep -Fq 'local ssh client not found; install an ssh client' || {
    echo "missing ssh client was not named: $output" >&2
    exit 1
}
if printf '%s\n' "$output" | grep -Fq 'os error'; then
    echo "missing ssh client leaked errno: $output" >&2
    exit 1
fi
if printf '%s\n' "$output" | grep -Fq 'whoami'; then
    echo "missing ssh client still mentioned whoami: $output" >&2
    exit 1
fi

echo "missing ssh client is named instead of leaking errno"
