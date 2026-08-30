#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
RUNNER=$ROOT/scripts/run-layer3-tests.sh

fail() {
    echo "layer3 runner check failed: $*" >&2
    exit 1
}

[ -f "$RUNNER" ] || fail "missing $RUNNER"

listed=$(grep -E '^[[:space:]]*--test ' "$RUNNER" | awk '{print $2}' | sort -u)

required_file=$(mktemp)
trap 'rm -f "$required_file"' EXIT

while IFS= read -r file; do
    rel=${file#"$ROOT"/}
    case "$rel" in
        */src/*)
            fail "$rel has an informing ignore in library code; list it in $RUNNER as --lib or drop the label"
            ;;
        */tests/*/*.rs)
            basename "$(dirname "$rel")"
            ;;
        */tests/*.rs)
            basename "$rel" .rs
            ;;
        *)
            fail "cannot map $rel to a cargo test binary"
            ;;
    esac
done < <(grep -rl --include='*.rs' 'ignore = "informing' "$ROOT/ployz" "$ROOT/ployz-testkit" "$ROOT/ployzd") |
    sort -u > "$required_file"

required=$(cat "$required_file")
missing=$(comm -23 <(printf '%s\n' "$required") <(printf '%s\n' "$listed"))
[ -z "$missing" ] || fail "informing binaries missing from run-layer3-tests.sh: $missing"

extra=$(comm -13 <(printf '%s\n' "$required") <(printf '%s\n' "$listed"))
[ -z "$extra" ] || fail "run-layer3-tests.sh lists binaries with no informing ignore: $extra"
