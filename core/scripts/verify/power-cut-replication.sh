#!/usr/bin/env bash
# Usage: scripts/verify/power-cut-replication.sh <manifest> [machine]
#
# Needs a run with at least 3 Machines, built with ployz/verify-faults for `ployz debug`, and a
# Service with a custom domain whose copies run on every Machine. Without DNS no certificate is
# issued and Caddy renders no route, so it first pins a self-signed certificate on each custom
# domain. Cuts power to <machine> (default: the last one), brings it back, cuts it again
# 8 s after boot so the cut lands on its own fresh Corrosion commits, then brings it back.
# With HOT_SERVICE=<service>, the first cut instead lands the moment `service restart` of that
# Service returns, on the commits the restart just made.
# From 60 s after the last boot, it fails when the Machines still hold different rows
# CONVERGE_SECONDS (default 120) later (rows-agree.sh, which needs sqlite3 on every Machine),
# or when any proxy routes an address on <machine> that is not an In copy there.
set -Eeuo pipefail
trap 'echo "could not run: line $LINENO"; exit 2' ERR
cd "$(dirname "$0")/../.."
run=$1
vc() { scripts/verify-cluster "$@"; }
target=${2:-$(jq -r '.machines[-1].name' "$run")}
target_id=$(jq -r --arg m "$target" '.machines[] | select(.name == $m) | .machine_id' "$run")
names=$(jq -r '.machines[].name' "$run")
[ "$(wc -w <<<"$names")" -ge 3 ] || { echo "needs at least 3 Machines"; exit 2; }
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
certify() {
  local dir host request
  dir=$(mktemp -d)
  for host in $(vc cli "$run" -- --json domain ls | jq -r '.domains[] | select(.kind == "custom") | .hostname'); do
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 2 -subj "/CN=$host" \
      -addext "subjectAltName=DNS:$host" -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
    request=$(jq -cn --arg host "$host" --rawfile chain "$dir/cert.pem" --rawfile key "$dir/key.pem" \
      '{protocol_major: 1, command: "publish_certificate_material", payload: {hostname: $host,
        change: {action: "set", certificate_chain_pem: $chain, private_key_pem: $key}}}')
    vc cli "$run" -- debug local-rpc "$request" >/dev/null
  done
  rm -rf "$dir"
}

for m in $names; do
  vc exec "$run" "$m" -- sh -c 'command -v sqlite3' >/dev/null || { echo "needs sqlite3 on $m"; exit 2; }
done
copies="select count(*) from containers where machine_id = '$target_id' and rotation = 'in'
  and namespace != 'ployz-system'"
actor="select hex(site_id) from crsql_site_id where ordinal = 0"
[ "$(store "$target" "$copies")" -gt 0 ] || { echo "needs a Service copy on $target"; exit 2; }
actor_before=$(store "$target" "$actor")
certify
if [ -n "${HOT_SERVICE:-}" ]; then
  vc cli "$run" -- service restart "$HOT_SERVICE" >/dev/null
fi
vc power off "$run" "$target" >/dev/null; down
vc power on "$run" "$target" >/dev/null
sleep 8
vc power off "$run" "$target" >/dev/null; down
vc power on "$run" "$target" >/dev/null
sleep 60

failed=0
[ "$(store "$target" "$copies")" -gt 0 ] || { echo "FAIL: $target holds no Service copy after the cuts"; failed=1; }
if [ "$(store "$target" "$actor")" = "$actor_before" ]; then
  echo "$target kept its Corrosion actor"
else
  echo "$target moved to a new Corrosion actor"
fi
scripts/verify/rows-agree.sh "$run" "$target" || { status=$?; [ "$status" = 1 ] || exit "$status"; failed=1; }

serving=$(store "$target" "select json_extract(container, '\$.address') from containers
  where machine_id = '$target_id' and rotation = 'in' and json_extract(container, '\$.address') is not null")
subnet=$(vc exec "$run" "$target" -- sh -c "ip -4 -o addr show | grep -o '10\.210\.[0-9]*\.' | head -1")
[ -n "$subnet" ] || { echo "no overlay address on $target"; exit 2; }
for m in $names; do
  routed=$(upstreams "$m") || { echo "FAIL: could not read $m's proxy routes"; failed=1; continue; }
  [ -n "$routed" ] || { echo "FAIL: $m's proxy routes nothing"; failed=1; continue; }
  for address in $(awk -v p="$subnet" 'index($0, p) == 1' <<<"$routed"); do
    if ! grep -qxF "$address" <<<"$serving"; then
      echo "FAIL: $m routes $address, which is not an In copy on $target"
      failed=1
    fi
  done
done

[ "$failed" = 0 ] && echo "PASS: every Machine holds the same rows and routes only In copies on $target"
exit "$failed"
