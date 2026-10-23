#!/usr/bin/env bash
# Usage: scripts/verify/rows-agree.sh <manifest> [reference machine]
#
# Waits up to CONVERGE_SECONDS (default 120) for every Machine to hold the same rows in every
# replicated table, every column compared, and fails with the differing rows when they never
# do. Equal clocks are not enough: a reused db_version can leave equal clocks on Machines that
# hold different values. Needs sqlite3 on every Machine.
set -Eeuo pipefail
trap 'echo "could not run: line $LINENO"; exit 2' ERR
cd "$(dirname "$0")/../.."
run=$1
vc() { scripts/verify-cluster "$@"; }
reference=${2:-$(jq -r '.machines[-1].name' "$run")}
names=$(jq -r '.machines[].name' "$run")
for m in $names; do
  vc exec "$run" "$m" -- sh -c 'command -v sqlite3' >/dev/null || { echo "needs sqlite3 on $m"; exit 2; }
done
tables="cluster key|machines id|containers id|certificates hostname|volumes machine_id, name|ingress_routes machine_id, service_id"
query=""
while IFS=' ' read -r table key; do
  query="$query select '$table' as tbl, * from $table order by $key;"
done < <(tr '|' '\n' <<<"$tables")
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
deadline=$(( SECONDS + ${CONVERGE_SECONDS:-120} ))
while :; do
  for m in $names; do
    vc exec "$run" "$m" -- sqlite3 -readonly -json /var/lib/ployz/corrosion/store.db "$query" >"$dir/$m"
  done
  differing=$(for m in $names; do cmp -s "$dir/$reference" "$dir/$m" || echo "$m"; done)
  if [ -z "$differing" ]; then
    echo "rows agree on every Machine: $(grep -c '"tbl":' "$dir/$reference") rows"
    exit 0
  fi
  [ "$SECONDS" -lt "$deadline" ] || break
  sleep 5
done
for m in $differing; do
  echo "FAIL: $m holds different rows from $reference after ${CONVERGE_SECONDS:-120}s:"
  python3 -I scripts/verify/rows_diff.py "$dir/$reference" "$dir/$m"
done
exit 1
