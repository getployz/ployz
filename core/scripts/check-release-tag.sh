#!/usr/bin/env bash

set -euo pipefail

tag=${1:-}
manifest=${2:-}

if [ -z "$tag" ]; then
    echo "usage: $0 <tag> [Cargo.toml]" >&2
    exit 1
fi

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=scripts/release-tag.sh
source "$root/scripts/release-tag.sh"
manifest=${manifest:-"$root/Cargo.toml"}

if ! stable_release_tag "$tag" && ! beta_release_tag "$tag"; then
    echo "tag '$tag' is not vX.Y.Z or vX.Y.Z-beta.N" >&2
    exit 1
fi

version=${tag#v}
workspace=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$manifest" | head -n1)
if [ "$workspace" != "$version" ]; then
    echo "tag '$tag' does not match workspace version '$workspace'" >&2
    exit 1
fi
