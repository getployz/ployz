#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=scripts/release-tag.sh
source "$ROOT/scripts/release-tag.sh"

fail() { echo "sdk publish failed: $1" >&2; exit 1; }

sdk_npm_publish_args() {
    local tag=$1 channel
    channel=$(channel_name_for_tag "$tag") || return 1
    printf '%s\n' --access public
    if [ "$channel" = beta ]; then
        printf '%s\n' --tag beta
    else
        printf '%s\n' --tag latest
    fi
}

publish_sdk_package() {
    local tag=$1 version dest cdylib packed
    mapfile -t publish_args < <(sdk_npm_publish_args "$tag")
    version=${tag#v}
    cargo build -p ployz-sdk --release --locked
    cdylib="$ROOT/target/release/libployz_sdk.so"
    [ -f "$cdylib" ] || fail "ployz-sdk cdylib was not produced"
    dest=$(mktemp -d)
    trap "rm -rf $(printf '%q' "$dest")" EXIT
    bash "$ROOT/scripts/pack-sdk-package.sh" "$dest" "$cdylib"
    packed=$(sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$dest/package.json" | head -n1)
    [ "$packed" = "$version" ] || fail "tag '$tag' does not match packed version '$packed'"
    if npm view "@ployz/sdk@${version}" version >/dev/null 2>&1; then
        echo "@ployz/sdk@${version} is already published"
        return 0
    fi
    npm publish "$dest" "${publish_args[@]}"
}

if [ "${PLOYZ_SDK_PUBLISH_TEST_ONLY:-false}" != true ]; then
    tag=${1:-}
    [ -n "$tag" ] || fail "usage: $0 <tag>"
    publish_sdk_package "$tag"
fi
