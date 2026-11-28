#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${1:-${DIST:-"$ROOT/dist"}}

# shellcheck source=scripts/release-archives.sh
source "$ROOT/scripts/release-archives.sh"

fail() { echo "release packing failed: $1" >&2; exit 1; }

cli_archives=$(release_archives ployz)
daemon_archives=$(release_archives ployzd)
all_archives=$(release_archives)

for archive in $all_archives; do
    [ -f "$DIST/$archive" ] || fail "missing $archive"
done

(
    cd "$DIST"
    # shellcheck disable=SC2086
    sha256sum $cli_archives $daemon_archives | sort -k2 > checksums.txt
)
