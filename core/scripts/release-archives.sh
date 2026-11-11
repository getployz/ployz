#!/usr/bin/env bash
# Reads crates/ployz-core/release-archives.txt, the one list of the archives a release publishes,
# which ployz-core reads too. Source this file; it sets no shell options.

RELEASE_ARCHIVES_FILE=${RELEASE_ARCHIVES_FILE:-"$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/crates/ployz-core/release-archives.txt"}

# release_archives [product [os [arch]]]: matching archive names in manifest order, one per line.
# An empty or missing filter matches everything. Fails when nothing matches.
release_archives() {
    local names
    names=$(awk -v product="${1:-}" -v os="${2:-}" -v arch="${3:-}" '
        $1 ~ /^#/ || NF == 0 { next }
        (product == "" || $1 == product) && (os == "" || $2 == os) && (arch == "" || $3 == arch) { print $4 }
    ' "$RELEASE_ARCHIVES_FILE") || return 1
    [ -n "$names" ] || {
        echo "$RELEASE_ARCHIVES_FILE has no archive for '${1:-}' '${2:-}' '${3:-}'" >&2
        return 1
    }
    printf '%s\n' "$names"
}

# release_archive product os arch: the one archive name for that platform.
release_archive() {
    release_archives "$1" "$2" "$3"
}

# release_archive_binaries archive: the binaries inside the archive, byte-sorted, one per line.
release_archive_binaries() {
    local binaries
    binaries=$(awk -v archive="$1" '$1 !~ /^#/ && $4 == archive { print $5 }' "$RELEASE_ARCHIVES_FILE") || return 1
    [ -n "$binaries" ] || {
        echo "$RELEASE_ARCHIVES_FILE does not list $1" >&2
        return 1
    }
    printf '%s\n' "$binaries" | tr ',' '\n' | LC_ALL=C sort
}
