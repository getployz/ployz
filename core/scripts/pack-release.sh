#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${1:-${DIST:-"$ROOT/dist"}}

# shellcheck source=scripts/homebrew-formula.sh
source "$ROOT/scripts/homebrew-formula.sh"
# shellcheck source=scripts/release-archives.sh
source "$ROOT/scripts/release-archives.sh"

fail() { echo "release packing failed: $1" >&2; exit 1; }

cli_archives=$(release_archives ployz)
daemon_archives=$(release_archives ployzd)
all_archives=$(release_archives)

for archive in $all_archives; do
    [ -f "$DIST/$archive" ] || fail "missing $archive"
done

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)
[ -n "$version" ] || fail "workspace version is missing"
tag=v$version
repo=${GITHUB_REPOSITORY:-getployz/ployz}

(
    cd "$DIST"
    # shellcheck disable=SC2086
    sha256sum $cli_archives $daemon_archives | sort -k2 > checksums.txt
)

write_homebrew_formula_from_checksums "$DIST/checksums.txt" "$DIST/homebrew/ployz.rb" "$version" "$tag" "$repo"
