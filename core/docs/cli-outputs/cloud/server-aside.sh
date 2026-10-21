# shellcheck shell=bash
# Sourced by cloud/run.sh and server-aside.test.sh; needs pg (the Postgres container) and org.
# set_server_aside moves the Organization's Server rows into capture_server, so
# Cloud counts no Server; restore_server moves them back and checks they are.
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
restore_server() {
  local after
  after=$(aside_sql "SET client_min_messages = warning;
    CREATE TABLE IF NOT EXISTS capture_server (LIKE organization_machine);
    INSERT INTO organization_machine SELECT * FROM capture_server ON CONFLICT DO NOTHING;
    DROP TABLE capture_server;
    SELECT count(*) || '|' || (to_regclass('capture_server') IS NULL) FROM organization_machine WHERE organization_id = '$org'") || return 1
  if [ "$after" != "${servers_before:?}|true" ]; then
    echo "restore_server: want $servers_before Server rows and no capture_server, found $after" >&2
    return 1
  fi
  server_aside=0
}
# What finish runs: puts a set-aside Server back, and fails when it can't.
restore_server_on_exit() {
  [ "$server_aside" = 1 ] || return 0
  if restore_server; then return 0; fi
  echo "restoring the seed's Server failed: Cloud counts no Server, and capture_server holds its row" >&2
  return 1
}
