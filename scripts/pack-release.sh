#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${1:-${DIST:-"$ROOT/dist"}}

# shellcheck source=scripts/homebrew-formula.sh
source "$ROOT/scripts/homebrew-formula.sh"

fail() { echo "release packing failed: $1" >&2; exit 1; }

cli_archives=$(printf '%s\n' \
    ployz_linux_amd64.tar.gz \
    ployz_linux_arm64.tar.gz \
    ployz_macos_amd64.tar.gz \
    ployz_macos_arm64.tar.gz)
all_archives=$(printf '%s\n' \
    $cli_archives \
    ployzd_linux_amd64.tar.gz \
    ployzd_linux_arm64.tar.gz)

for archive in $all_archives; do
    [ -f "$DIST/$archive" ] || fail "missing $archive"
done

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
[ -n "$version" ] || fail "workspace version is missing"
tag=${GITHUB_REF_NAME:-v$version}
[[ "$tag" == v* ]] || tag="v$tag"
repo=${GITHUB_REPOSITORY:-getployz/ployz2}

(
    cd "$DIST"
    # shellcheck disable=SC2086
    sha256sum $cli_archives | sort -k2 > checksums.txt
)

write_homebrew_formula_from_checksums "$DIST/checksums.txt" "$DIST/homebrew/ployz.rb" "$version" "$tag" "$repo"
