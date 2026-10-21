# shellcheck shell=bash
# Sourced by cloud/run.sh and server-aside.test.sh; needs pg (the Postgres container) and org.
# set_server_aside moves the Organization's Server rows into capture_server, so
# Cloud counts no Server; restore_server moves them back and checks they are.
# The capture owns its verify Cloud: nothing else adds or removes the
# Organization's Servers meanwhile, so the count before is the count to restore.
server_aside=0
# Its own session: a signal to the driver's process group can't kill the client
# mid-transaction, and the driver's traps run only after it returns.
aside_sql() { setsid -w docker exec "${pg:?}" psql -qtA -v ON_ERROR_STOP=1 -U postgres -d ployz_cloud -c "$1"; }
set_server_aside() {
  servers_before=$(aside_sql "SELECT count(*) FROM organization_machine WHERE organization_id = '${org:?}'") || return 1
  server_aside=1
  aside_sql "CREATE TABLE capture_server AS SELECT * FROM organization_machine WHERE organization_id = '$org';
    DELETE FROM organization_machine WHERE organization_id = '$org'" >/dev/null
}
# The Organization's Server rows and whether capture_server exists, as `N|true`.
aside_state() {
  aside_sql "SELECT count(*) || '|' || (to_regclass('capture_server') IS NOT NULL) FROM organization_machine WHERE organization_id = '$org'"
}
restore_server() {
  local after
  after=$(aside_sql "SET client_min_messages = warning;
    CREATE TABLE IF NOT EXISTS capture_server (LIKE organization_machine);
    INSERT INTO organization_machine SELECT * FROM capture_server ON CONFLICT DO NOTHING;
    DROP TABLE capture_server;
    SELECT count(*) || '|' || (to_regclass('capture_server') IS NOT NULL) FROM organization_machine WHERE organization_id = '$org'") || return 1
  if [ "$after" != "${servers_before:?}|false" ]; then
    echo "restore_server: want $servers_before Server rows and no capture_server, found ${after%|*} rows and capture_server present: ${after#*|}" >&2
    return 1
  fi
  server_aside=0
}
restore_server_on_exit() {
  [ "$server_aside" = 1 ] || return 0
  if restore_server; then return 0; fi
  local state
  if state=$(aside_state); then
    echo "restoring the seed's Server failed: the Organization has ${state%|*} Server rows (${servers_before:-?} before), capture_server present: ${state#*|}" >&2
  else
    echo "restoring the seed's Server failed, and its state can't be read" >&2
  fi
  return 1
}

# The driver's INT, TERM and EXIT traps. Each signal trap ignores further INT and
# TERM before it exits, and so does the exit trap, so a second signal can't cut
# the restore short. The exit trap calls the driver's stop_cloud only once no
# Server is left aside; otherwise it calls keep_cloud, so the state stays to
# inspect, and exits non-zero.
capture_traps() {
  trap 'trap "" INT TERM; exit 130' INT
  trap 'trap "" INT TERM; exit 143' TERM
  trap capture_exit EXIT
}
capture_exit() {
  local status=$?
  trap '' INT TERM
  if restore_server_on_exit; then
    stop_cloud
  else
    [ "$status" != 0 ] || status=1
    keep_cloud
  fi
  exit "$status"
}
