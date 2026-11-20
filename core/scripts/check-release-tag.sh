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

# Servers mark origins before they check them, and a build that marks without checking
# would run born data under a Legacy label. No release carries one: the catalog declares
# the capability, and every build's pulls claim it. The claim is read as written, so any
# other spelling holds the tag until this check learns it.
if grep -q '^impl VolumeOrigin' "$root/crates/ployz-core/src/domain/volume.rs"; then
    if ! grep -Eq '^[[:space:]]*[A-Z_]+: \("ployz\.volume\.origin\.v1",' "$root/crates/ployz-core/src/rpc_catalog.rs"; then
        echo "tag '$tag' carries Volume origins, but the catalog does not declare ployz.volume.origin.v1" >&2
        exit 1
    fi
    claims="$root/crates/ployzd/src/volume_plugin/transfer.rs"
    if [ "$(grep -Ec 'CLAIMS_ORIGIN: bool' "$claims")" != 1 ] || ! grep -Eq '^const CLAIMS_ORIGIN: bool = true;$' "$claims"; then
        echo "tag '$tag' carries Volume origins, but its pulls do not claim ployz.volume.origin.v1 in every build" >&2
        exit 1
    fi
fi
