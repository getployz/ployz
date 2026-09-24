#!/usr/bin/env bash

set -euo pipefail

# A semver numeric identifier has no leading zeros and fits a u64 (at most 19 digits here); the
# daemon's version parser refuses anything else.
# install.sh keeps the same grammar.
RELEASE_NUMBER='(0|[1-9][0-9]{0,18})'

beta_release_tag() {
    [[ "$1" =~ ^v$RELEASE_NUMBER\.$RELEASE_NUMBER\.$RELEASE_NUMBER-beta\.$RELEASE_NUMBER$ ]]
}

stable_release_tag() {
    [[ "$1" =~ ^v$RELEASE_NUMBER\.$RELEASE_NUMBER\.$RELEASE_NUMBER$ ]]
}

release_tag() {
    stable_release_tag "$1" || beta_release_tag "$1"
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

# Succeeds when release tag $1 is higher than $2 by semver. In GNU `sort -V`, `~` sorts before
# the bare version, so vX.Y.Z~beta.N < vX.Y.Z.
release_tag_higher() {
    local higher=${1/-beta./\~beta.} lower=${2/-beta./\~beta.}
    [ "$higher" != "$lower" ] && [ "$(printf '%s\n' "$lower" "$higher" | sort -V | tail -n1)" = "$higher" ]
}
