#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${DIST:-"$ROOT/dist"}
EXPECTED_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
IMAGE=${PLOYZ_RELAY_IMAGE:-ghcr.io/getployz/ployz-relay:verify}

fail() { echo "relay image verification failed: $1" >&2; exit 1; }

dockerfile=$ROOT/ployz-relay/Dockerfile
grep -Fxq 'FROM scratch' "$dockerfile" || fail "Dockerfile is not FROM scratch"
if grep -Eq '^RUN |^ADD ' "$dockerfile"; then
    fail "Dockerfile must COPY the binary only"
fi
archive=$DIST/ployz-relay_linux_amd64.tar.gz
[ -f "$archive" ] || fail "missing $archive"

extract=$(mktemp -d)
trap 'rm -rf "$extract"' EXIT
tar -xzf "$archive" -C "$extract"
bash "$ROOT/scripts/build-relay-image.sh" "$extract/ployz-relay" "$IMAGE" linux/amd64
output=$(docker run --rm "$IMAGE" version)
[ "$output" = "$EXPECTED_VERSION" ] || fail "image version was '$output'"
echo "relay image verified"
