#!/usr/bin/env bash

set -euo pipefail

beta_release_tag() {
    [[ "$1" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-beta\.[0-9]+$ ]]
}

stable_release_tag() {
    [[ "$1" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

channel_name_for_tag() {
    local tag=$1
    if beta_release_tag "$tag"; then
        printf 'beta\n'
    elif stable_release_tag "$tag"; then
        printf 'stable\n'
    else
        echo "tag '$tag' is not vX.Y.Z or vX.Y.Z-beta.N" >&2
        return 1
    fi
}
