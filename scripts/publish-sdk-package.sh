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

publish_npm_package() {
    local dir=$1 name=$2 version=$3
    if npm view "${name}@${version}" version >/dev/null 2>&1; then
        echo "${name}@${version} is already published"
        return 0
    fi
    npm publish "$dir" "${publish_args[@]}"
}

publish_sdk_package() {
    local tag=$1 version dest packed
    shift
    mapfile -t publish_args < <(sdk_npm_publish_args "$tag")
    version=${tag#v}
    [ "$#" -gt 0 ] || fail "every declared binding must be passed in"
    dest=$(mktemp -d)
    trap "rm -rf $(printf '%q' "$dest")" EXIT
    bash "$ROOT/scripts/pack-sdk-package.sh" "$dest" "$@"
    packed=$(sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$dest/package.json" | head -n1)
    [ "$packed" = "$version" ] || fail "tag '$tag' does not match packed version '$packed'"
    # Bindings first: @ployz/sdk is unusable until its optional dependencies resolve.
    for binding_package in "$dest"/npm/*; do
        publish_npm_package "$binding_package" "@ployz/sdk-$(basename "$binding_package")" "$version"
    done
    publish_npm_package "$dest" @ployz/sdk "$version"
}

if [ "${PLOYZ_SDK_PUBLISH_TEST_ONLY:-false}" != true ]; then
    tag=${1:-}
    [ -n "$tag" ] || fail "usage: $0 <tag> <binding>..."
    shift
    publish_sdk_package "$tag" "$@"
fi
