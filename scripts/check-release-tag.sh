#!/usr/bin/env bash

set -euo pipefail

tag=${1:-}
manifest=${2:-}
sdk_package=${3:-}

if [ -z "$tag" ]; then
    echo "usage: $0 <tag> [Cargo.toml] [package.json]" >&2
    exit 1
fi

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=scripts/release-tag.sh
source "$root/scripts/release-tag.sh"
manifest=${manifest:-"$root/Cargo.toml"}
sdk_package=${sdk_package:-"$root/ployz-sdk/package.json"}

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

if [ ! -f "$sdk_package" ]; then
    echo "sdk package '$sdk_package' is missing" >&2
    exit 1
fi
sdk_version=$(sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$sdk_package" | head -n1)
if [ "$sdk_version" != "$version" ]; then
    echo "tag '$tag' does not match sdk package version '$sdk_version'" >&2
    exit 1
fi
