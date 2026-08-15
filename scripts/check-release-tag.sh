#!/usr/bin/env bash

set -euo pipefail

tag=${1:-}
manifest=${2:-}

if [ -z "$tag" ]; then
    echo "usage: $0 <tag> [Cargo.toml]" >&2
    exit 1
fi

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
manifest=${manifest:-"$root/Cargo.toml"}

if ! [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "tag '$tag' is not a stable SemVer vX.Y.Z" >&2
    exit 1
fi

version=${tag#v}
workspace=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$manifest" | head -n1)
if [ "$workspace" != "$version" ]; then
    echo "tag '$tag' does not match workspace version '$workspace'" >&2
    exit 1
fi
