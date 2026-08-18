#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${DIST:-"$ROOT/dist"}
IMAGE_REPO=${PLOYZ_RELAY_IMAGE_REPO:-ghcr.io/getployz/ployz-relay}
tag=${1:-${GITHUB_REF_NAME:-}}

fail() { echo "relay image publish failed: $1" >&2; exit 1; }

[ -n "$tag" ] || fail "usage: $0 <tag>"

extract=$(mktemp -d)
trap 'rm -rf "$extract"' EXIT

publish_arch() {
    local archive=$1 platform=$2 suffix=$3
    [ -f "$DIST/$archive" ] || fail "missing $DIST/$archive"
    mkdir -p "$extract/$suffix"
    tar -xzf "$DIST/$archive" -C "$extract/$suffix"
    bash "$ROOT/scripts/build-relay-image.sh" \
        "$extract/$suffix/ployz-relay" \
        "$IMAGE_REPO:$tag-$suffix" \
        "$platform"
    docker push "$IMAGE_REPO:$tag-$suffix"
}

publish_arch ployz-relay_linux_amd64.tar.gz linux/amd64 amd64
publish_arch ployz-relay_linux_arm64.tar.gz linux/arm64 arm64
docker buildx imagetools create \
    -t "$IMAGE_REPO:$tag" \
    "$IMAGE_REPO:$tag-amd64" \
    "$IMAGE_REPO:$tag-arm64"
echo "published $IMAGE_REPO:$tag"
