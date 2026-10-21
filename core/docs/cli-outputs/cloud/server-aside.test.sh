#!/usr/bin/env bash
# Checks server-aside.sh against a disposable Postgres: a driver interrupted by
# SIGINT or SIGTERM to its process group while its set-aside transaction waits
# on a lock still ends with the Server row back and no capture_server, and a
# restore that fails makes the driver exit non-zero. Needs Docker.
set -euo pipefail
HERE=$(cd -- "$(dirname -- "$0")" && pwd)
cid=$(docker run -d --rm -e POSTGRES_PASSWORD=x -e POSTGRES_DB=ployz_cloud postgres:16-alpine)
trap 'docker stop "$cid" >/dev/null' EXIT
q() { docker exec "$cid" psql -qtA -v ON_ERROR_STOP=1 -U postgres -d ployz_cloud -c "$1"; }
for _ in $(seq 60); do q 'SELECT 1' >/dev/null 2>&1 && break; sleep 1; done
q "CREATE TABLE organization_machine (organization_id text, machine_id text, PRIMARY KEY (organization_id, machine_id))"

fails=0
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: got '$2', want '$3'"; fails=1; fi
}
reset() {
  q "SET client_min_messages = warning; DROP TABLE IF EXISTS capture_server; TRUNCATE organization_machine;
    INSERT INTO organization_machine VALUES ('o', 'm'), ('other', 'x')"
}
state() { q "SELECT count(*) FILTER (WHERE organization_id = 'o') || ' o, ' || count(*) || ' all, side table ' || (to_regclass('capture_server') IS NOT NULL) FROM organization_machine"; }
# Job control: the driver gets its own process group and doesn't start with SIGINT ignored.
set -m
# driver BODY: a stand-in for cloud/run.sh, in its own process group, with its traps and finish.
driver() {
  bash -c 'set -euo pipefail; source "$1"; pg=$2; org=o
    trap "exit 130" INT; trap "exit 143" TERM
    trap "status=\$?; if ! restore_server_on_exit && [ \$status = 0 ]; then status=1; fi; exit \$status" EXIT
    eval "$3"' _ "$HERE/server-aside.sh" "$cid" "$1" &
}

# interrupted SIG: sets result. The set-aside's DELETE waits on a SHARE lock while the signal reaches the driver's group.
interrupted() {
  reset
  docker exec "$cid" psql -q -U postgres -d ployz_cloud \
    -c "BEGIN; LOCK organization_machine IN SHARE MODE; SELECT pg_sleep(6); COMMIT" >/dev/null &
  local locker=$! pid status=0
  until [ "$(q "SELECT count(*) FROM pg_locks WHERE mode = 'ShareLock' AND relation = 'organization_machine'::regclass AND granted")" = 1 ]; do sleep 0.2; done
  driver 'set_server_aside; sleep 30'
  pid=$!
  until [ "$(q "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' AND query LIKE '%capture_server%'")" = 1 ]; do sleep 0.2; done
  kill -"$1" -- -"$pid"
  wait "$pid" || status=$?
  wait "$locker"
  sleep 1
  result="$status; $(state)"
}

interrupted INT
check "SIGINT mid set-aside: the Server is back" "$result" "130; 1 o, 2 all, side table false"
interrupted TERM
check "SIGTERM mid set-aside: the Server is back" "$result" "143; 1 o, 2 all, side table false"

reset
status=0; driver 'set_server_aside; restore_server'; wait $! || status=$?
check "set aside then restored" "$status; $(state)" "0; 1 o, 2 all, side table false"

reset
status=0; driver 'set_server_aside; pg=no-such-container'; wait $! 2>/dev/null || status=$?
check "a restore that fails fails the driver" "$([ "$status" != 0 ] && echo failed || echo "exit 0"); $(state)" "failed; 0 o, 1 all, side table true"

exit "$fails"
