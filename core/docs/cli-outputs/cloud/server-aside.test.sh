#!/usr/bin/env bash
# shellcheck disable=SC2016 # driver bodies and wait_for conditions expand when they run
# Checks server-aside.sh against a disposable Postgres:
# - a driver interrupted by SIGINT or SIGTERM to its process group while its
#   set-aside transaction waits on a lock still ends with the Server row back
#   and no capture_server;
# - a second signal once the exit trap has begun doesn't stop the restore;
# - a restore that fails makes the driver exit non-zero and keeps Cloud up:
#   keep_cloud runs, never stop_cloud.
# Needs Docker.
set -euo pipefail
HERE=$(cd -- "$(dirname -- "$0")" && pwd)
cid=$(docker run -d --rm -e POSTGRES_PASSWORD=x -e POSTGRES_DB=ployz_cloud postgres:16-alpine)
work=$(mktemp -d)
trap 'docker stop "$cid" >/dev/null; rm -rf "$work"' EXIT
q() { docker exec "$cid" psql -qtA -v ON_ERROR_STOP=1 -U postgres -d ployz_cloud -c "$1"; }
for _ in $(seq 60); do q 'SELECT 1' >/dev/null 2>&1 && break; sleep 1; done
q "CREATE TABLE organization_machine (organization_id text, machine_id text, PRIMARY KEY (organization_id, machine_id))"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: got '$2', want '$3'"; fails=1; fi
}
reset() {
  mark=$work/$1; mkdir -p "$mark"
  q "SET client_min_messages = warning; DROP TABLE IF EXISTS capture_server; TRUNCATE organization_machine;
    INSERT INTO organization_machine VALUES ('o', 'm'), ('other', 'x')"
}
# STATUS; Server rows in o, in all; side table; which Cloud hook ran.
result() {
  local hooks='' hook
  for hook in stopped kept; do [ ! -e "$mark/$hook" ] || hooks+="$hook "; done
  echo "$1; $(q "SELECT count(*) FILTER (WHERE organization_id = 'o') || ' o, ' || count(*) || ' all, side table ' || (to_regclass('capture_server') IS NOT NULL) FROM organization_machine"); ${hooks:-no hook}"
}
# Job control: the driver gets its own process group and doesn't start with SIGINT ignored.
set -m
# driver BODY: a stand-in for cloud/run.sh with its traps and Cloud hooks; the hooks only leave a mark.
driver() {
  bash -c 'set -euo pipefail; source "$1"; pg=$2; org=o; mark=$4
    stop_cloud() { : >"$mark/stopped"; }
    keep_cloud() { : >"$mark/kept"; }
    capture_traps
    eval "$3"' _ "$HERE/server-aside.sh" "$cid" "$1" "$mark" &
}
# Holds the exit trap at restore_server until the test touches $mark/go.
barrier='eval "$(declare -f restore_server | sed 1s/restore_server/unbarred_restore_server/)"
  restore_server() { : >"$mark/entered"; until [ -e "$mark/go" ]; do sleep 0.1; done; unbarred_restore_server; }'
wait_for() { for _ in $(seq 300); do eval "$1" && return; sleep 0.1; done; echo "timed out: $1" >&2; return 1; }

# interrupted SIG: sets out. The set-aside's DELETE waits on a SHARE lock while the signal reaches the driver's group.
interrupted() {
  reset "interrupted-$1"
  docker exec "$cid" psql -q -U postgres -d ployz_cloud \
    -c "BEGIN; LOCK organization_machine IN SHARE MODE; SELECT pg_sleep(6); COMMIT" >/dev/null &
  local locker=$! pid status=0
  wait_for '[ "$(q "SELECT count(*) FROM pg_locks WHERE mode = '"'ShareLock'"' AND relation = '"'organization_machine'"'::regclass AND granted")" = 1 ]'
  driver 'set_server_aside; sleep 30'
  pid=$!
  wait_for '[ "$(q "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = '"'Lock'"' AND query LIKE '"'%capture_server%'"'")" = 1 ]'
  kill -"$1" -- -"$pid"
  wait "$pid" || status=$?
  wait "$locker"
  out=$(result "$status")
}
# twice FIRST SECOND: sets out. FIRST starts the exit trap; SECOND lands while it waits at restore_server.
twice() {
  reset "twice-$1-$2"
  local pid status=0
  driver "set_server_aside; $barrier; sleep 30"
  pid=$!
  wait_for '[ "$(q "SELECT to_regclass('"'capture_server'"') IS NOT NULL")" = t ]'
  kill -"$1" -- -"$pid"
  wait_for '[ -e "$mark/entered" ]'
  kill -"$2" -- -"$pid" 2>/dev/null || true
  sleep 0.5
  : >"$mark/go"
  wait "$pid" || status=$?
  out=$(result "$status")
}

interrupted INT
check "SIGINT mid set-aside: the Server is back" "$out" "130; 1 o, 2 all, side table false; stopped "
interrupted TERM
check "SIGTERM mid set-aside: the Server is back" "$out" "143; 1 o, 2 all, side table false; stopped "
twice INT TERM
check "SIGINT, then SIGTERM during the restore: the Server is back" "$out" "130; 1 o, 2 all, side table false; stopped "
twice TERM INT
check "SIGTERM, then SIGINT during the restore: the Server is back" "$out" "143; 1 o, 2 all, side table false; stopped "

reset restored
status=0; driver 'set_server_aside; restore_server'; wait $! || status=$?
check "set aside then restored" "$(result "$status")" "0; 1 o, 2 all, side table false; stopped "

reset failed
status=0; driver 'set_server_aside; pg=no-such-container'; wait $! 2>"$mark/err" || status=$?
check "a restore that fails fails the driver and keeps Cloud up" \
  "$([ "$status" != 0 ] && echo failed || echo "exit 0");$(result x | cut -d';' -f2-)" "failed; 0 o, 1 all, side table true; kept "

exit "$fails"
