#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

fail() { echo "relay image build failed: $1" >&2; exit 1; }

[ "${1:-}" = --help ] && {
    echo "usage: $0 <binary> <tag> [platform]" >&2
    exit 1
}

binary=${1:-}
tag=${2:-}
platform=${3:-linux/amd64}

[ -n "$binary" ] && [ -n "$tag" ] || fail "usage: $0 <binary> <tag> [platform]"
[ -f "$binary" ] || fail "missing binary $binary"

context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
install -m 0755 "$binary" "$context/ployz-relay"
docker build --platform "$platform" -f "$ROOT/ployz-relay/Dockerfile" -t "$tag" "$context"
