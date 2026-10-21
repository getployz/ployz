#!/usr/bin/env bash
# Usage: scripts/verify/power-cut-replication.sh <manifest> [machine]
#
# Needs a run with at least 2 Machines and a Service with a domain whose copies run on every
# Machine. Cuts power to <machine> (default: the last one), brings it back, cuts it again
# 8 s after boot so the cut lands on its own fresh Corrosion commits, then brings it back.
# 60 s later it fails when any Machine holds a different clock or value for one of <machine>'s
# container rows, or when any proxy routes an address on <machine> that is not an In copy there.
set -euo pipefail
trap 'echo "could not run: line $LINENO"; exit 2' ERR
cd "$(dirname "$0")/../.."
run=$1
vc() { scripts/verify-cluster "$@"; }
target=${2:-$(jq -r '.machines[-1].name' "$run")}
target_id=$(jq -r --arg m "$target" '.machines[] | select(.name == $m) | .machine_id' "$run")
names=$(jq -r '.machines[].name' "$run")
store() { vc exec "$run" "$1" -- sqlite3 -readonly /var/lib/ployz/corrosion/store.db "$2"; }
upstreams() {
  vc exec "$run" "$1" -- curl -sf --unix-socket /run/ployz/ingress/caddy/admin.sock http://localhost/config/ \
    | jq -r '[..|objects|select(.handler=="reverse_proxy")|.upstreams[].dial|sub(":[0-9]+$"; "")]|.[]'
}
down() {
  for _ in $(seq 90); do
    vc cli "$run" -- --json server ls 2>/dev/null \
      | jq -e --arg m "$target" '[.servers[]|select(.machine.name==$m)|.membership]|.[0]=="down"' >/dev/null && return
    sleep 2
  done
  echo "$target never showed as down"
  exit 2
}

vc power off "$run" "$target" >/dev/null; down
vc power on "$run" "$target" >/dev/null
sleep 8
vc power off "$run" "$target" >/dev/null; down
vc power on "$run" "$target" >/dev/null
sleep 60

clocks="select p.id||' '||c.col_name||' '||c.col_version||' '||c.db_version
  from containers__crsql_clock c join containers__crsql_pks p on p.__crsql_key = c.key
  join containers t on t.id = p.id where t.machine_id = '$target_id' order by 1"
contents="select id||' '||gen||' '||rotation||' '||coalesce(json_extract(container, '\$.address'), '')
  from containers where machine_id = '$target_id' order by 1"
failed=0
for query in "$clocks" "$contents"; do
  reference=$(store "$target" "$query")
  for m in $names; do
    [ "$m" = "$target" ] && continue
    if ! diff <(echo "$reference") <(store "$m" "$query") >/dev/null; then
      echo "FAIL: $m disagrees with $target about $target's containers:"
      diff <(echo "$reference") <(store "$m" "$query") || true
      failed=1
    fi
  done
done

serving=$(store "$target" "select json_extract(container, '\$.address') from containers
  where machine_id = '$target_id' and rotation = 'in' and json_extract(container, '\$.address') is not null")
subnet=$(vc exec "$run" "$target" -- sh -c "ip -4 -o addr show | grep -o '10\.210\.[0-9]*\.' | head -1")
[ -n "$subnet" ] || { echo "no overlay address on $target"; exit 2; }
for m in $names; do
  routed=$(upstreams "$m") || { echo "FAIL: could not read $m's proxy routes"; failed=1; continue; }
  for address in $(awk -v p="$subnet" 'index($0, p) == 1' <<<"$routed"); do
    if ! grep -qxF "$address" <<<"$serving"; then
      echo "FAIL: $m routes $address, which is not an In copy on $target"
      failed=1
    fi
  done
done

[ "$failed" = 0 ] && echo "PASS: every Machine agrees on $target's containers and routes only its In copies"
exit "$failed"
