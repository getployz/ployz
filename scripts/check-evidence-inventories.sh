#!/bin/sh
set -eu

unset CDPATH
repo_root=$(cd "$(dirname "$0")/.." && pwd)
todo_ledger="$repo_root/UPSTREAM_TODOS.md"
layer1="$repo_root/evidence/layer1.tsv"
layer3="$repo_root/evidence/layer3.tsv"
baseline=b7e224a1eff98813b1d1a32034d977be24be994e

fail() {
  echo "evidence check failed: $*" >&2
  exit 1
}

verify_rust_evidence() {
  key=$1
  locator=$2
  path=${locator%%::*}
  test_name=${locator#*::}
  [ "$path" != "$locator" ] || fail "$key has a malformed Rust evidence locator"
  [ -f "$repo_root/$path" ] || fail "$key references missing Rust evidence file $path"
  grep -Eq "^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+${test_name}[[:space:]]*\(" "$repo_root/$path" \
    || fail "$key references missing Rust test $locator"
}

explicit_count=$(grep -Ec '^\| UT-[0-9]{3} \|' "$todo_ledger")
[ "$explicit_count" -eq 151 ] || fail "expected 151 explicit TODO rows, found $explicit_count"

omission_count=$(grep -Ec '^\| EO-[0-9]{3} \|' "$todo_ledger")
[ "$omission_count" -eq 19 ] || fail "expected 19 equivalent omission rows, found $omission_count"

ledger_rows=$(grep -E '^\| (UT|EO)-[0-9]{3} \|' "$todo_ledger")
unpinned=$(printf '%s\n' "$ledger_rows" | grep -Fvc "$baseline" || true)
[ "$unpinned" -eq 0 ] || fail "$unpinned TODO ledger rows are not pinned to the baseline"

bad_dispositions=$(printf '%s\n' "$ledger_rows" | grep -Evc '\| (Preserve boundary|Carry TODO|Resolve by Rust structure|Migration cleanup / not applicable|Reference only) \|' || true)
[ "$bad_dispositions" -eq 0 ] || fail "$bad_dispositions TODO ledger rows have an invalid disposition"

bad_ledger_keys=$(printf '%s\n' "$ledger_rows" | awk -F '|' '
  {
    key = $2
    gsub(/^ +| +$/, "", key)
    prefix = substr(key, 1, 2)
    count[prefix]++
    expected = sprintf("%s-%03d", prefix, count[prefix])
    if (key != expected) bad++
  }
  END {print bad + 0}
')
[ "$bad_ledger_keys" -eq 0 ] || fail "$bad_ledger_keys TODO ledger keys are duplicate or out of sequence"

pending_locations=$(printf '%s\n' "$ledger_rows" | grep -Fc 'Pending — owning implementation slice' || true)
[ "$pending_locations" -eq 0 ] || fail "$pending_locations TODO ledger rows still have a pending location"

layer1_count=$(awk -F '\t' 'NR > 1 && NF {count++} END {print count + 0}' "$layer1")
[ "$layer1_count" -eq 86 ] || fail "expected 86 Layer 1 semantic cases, found $layer1_count"

bad_layer1=$(awk -F '\t' -v baseline="$baseline" '
  NR == 1 {
    if ($0 != "key\tfamily\tupstream_case\tpinned_source\tdisposition\trust_evidence") bad++
    next
  }
  $1 != sprintf("L1-%03d", NR - 1) {bad++}
  $1 !~ /^L1-[0-9][0-9][0-9]$/ {bad++}
  index($4, baseline) == 0 {bad++}
  $5 !~ /^(tested|not-ported)$/ {bad++}
  $5 == "tested" && $6 !~ /\.rs::[a-zA-Z0-9_]+$/ {bad++}
  $5 == "not-ported" && $6 == "" {bad++}
  END {print bad + 0}
' "$layer1")
[ "$bad_layer1" -eq 0 ] || fail "$bad_layer1 malformed Layer 1 fields"

tab=$(printf '\t')
while IFS="$tab" read -r key _family _upstream_case _source disposition rust_evidence; do
  [ "$key" = "key" ] && continue
  [ "$disposition" = "tested" ] && verify_rust_evidence "$key" "$rust_evidence"
done < "$layer1"

layer3_count=$(awk -F '\t' 'NR > 1 && NF {count++} END {print count + 0}' "$layer3")
selected_count=$(awk -F '\t' 'NR > 1 && $4 == "selected" {count++} END {print count + 0}' "$layer3")
not_ported_count=$((layer3_count - selected_count))
[ "$layer3_count" -eq 76 ] || fail "expected 76 Layer 3 declarations, found $layer3_count"
[ "$selected_count" -eq 60 ] || fail "expected 60 selected Layer 3 declarations, found $selected_count"
[ "$not_ported_count" -eq 16 ] || fail "expected 16 not-ported Layer 3 declarations, found $not_ported_count"

bad_layer3=$(awk -F '\t' -v baseline="$baseline" '
  NR == 1 {
    if ($0 != "key\tdeclaration\tpinned_source\tdisposition\treason") bad++
    next
  }
  $1 != sprintf("L3-%03d", NR - 1) {bad++}
  $1 !~ /^L3-[0-9][0-9][0-9]$/ {bad++}
  index($3, baseline) == 0 {bad++}
  $4 !~ /^(selected|harness-only|layer-1|consolidated|internal|setup-parent)$/ {bad++}
  END {print bad + 0}
' "$layer3")
[ "$bad_layer3" -eq 0 ] || fail "$bad_layer3 malformed Layer 3 fields"

unmapped_layer3=$(awk -F '\t' 'NR > 1 && $4 == "selected" && $5 !~ /\.rs::[a-zA-Z0-9_]+\.$/ {count++} END {print count + 0}' "$layer3")
[ "$unmapped_layer3" -eq 0 ] || fail "$unmapped_layer3 selected Layer 3 declarations lack exact Rust evidence"

while IFS="$tab" read -r key _declaration _source disposition reason; do
  [ "$key" = "key" ] && continue
  if [ "$disposition" = "selected" ]; then
    locator=${reason#Covered by }
    locator=${locator%.}
    verify_rust_evidence "$key" "$locator"
  fi
done < "$layer3"

fixture_count=$(find "$repo_root/evidence/upstream/e2e-fixtures" -type f | wc -l | tr -d ' ')
reference_count=$(find "$repo_root/evidence/upstream/cli-reference" -type f | wc -l | tr -d ' ')
[ "$fixture_count" -eq 13 ] || fail "expected 13 fixture files, found $fixture_count"
[ "$reference_count" -eq 58 ] || fail "expected 58 command-reference pages, found $reference_count"

bad_deviation_lines=$(grep -Evc '^- .+' "$repo_root/CLI_DEVIATIONS.md" || true)
[ "$bad_deviation_lines" -eq 0 ] || fail "$bad_deviation_lines malformed CLI deviation lines"

(
  cd "$repo_root/evidence"
  sha256sum --check --quiet SHA256SUMS
)

echo "evidence inventories verified: 151 TODO markers, 19 equivalent omissions, 86 Layer 1 semantic cases, 76 Layer 3 declarations (60 selected, 16 not ported), 13 fixtures, 58 command pages"
