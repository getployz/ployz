# Sourced first by the scripts that start a verify Cloud's long-lived processes (up.sh, worker.sh). An agent session
# runs in a systemd scope, and restarting the session stops every process in it, so a Cloud started there died with
# its session. This runs the script again in a transient user scope of its own. With --scope, systemd-run runs the
# command itself, so it keeps the caller's environment, PATH and HOME among it. Without a user systemd, as on macOS,
# the script runs where it was started.
if [ -z "${VERIFY_SCOPED:-}" ] && command -v systemd-run >/dev/null && systemd-run --user --scope --quiet true 2>/dev/null; then
  VERIFY_SCOPED=1 exec systemd-run --user --scope --quiet --collect -- "$BASH" "$0" "$@"
fi
export VERIFY_SCOPED=1
