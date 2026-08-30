#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TSV=$ROOT/evidence/product-paths.tsv

fail() {
    echo "product-paths check failed: $*" >&2
    exit 1
}

[ -f "$TSV" ] || fail "missing $TSV"

header=$(head -n1 "$TSV")
[ "$header" = $'path\trung1\trung2\trung3\trung4\trung5' ] || fail "unexpected header: $header"

verify_locator() {
    key=$1
    locator=$2
    case "$locator" in
        - | gap) return 0 ;;
        scripts/*)
            [ -f "$ROOT/$locator" ] || fail "$key references missing $locator"
            ;;
        *.rs::*)
            path=${locator%%::*}
            test_name=${locator#*::}
            [ -f "$ROOT/$path" ] || fail "$key references missing $path"
            grep -Eq "^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+${test_name}[[:space:]]*\(" "$ROOT/$path" \
                || fail "$key references missing $locator"
            ;;
        *)
            fail "$key has an unknown locator $locator"
            ;;
    esac
}

awk -F '\t' 'NR > 1 && NF {print}' "$TSV" | while IFS=$'\t' read -r path r1 r2 r3 r4 r5; do
    [ -n "$path" ] || fail "empty path"
    [ -n "$r1" ] && [ -n "$r2" ] && [ -n "$r3" ] && [ -n "$r4" ] && [ -n "$r5" ] || fail "$path has an empty rung cell"
    verify_locator "$path.rung1" "$r1"
    verify_locator "$path.rung2" "$r2"
    verify_locator "$path.rung3" "$r3"
    verify_locator "$path.rung4" "$r4"
    verify_locator "$path.rung5" "$r5"
done

count=$(awk -F '\t' 'NR > 1 && NF {count++} END {print count + 0}' "$TSV")
[ "$count" -ge 5 ] || fail "expected at least 5 product paths, found $count"
