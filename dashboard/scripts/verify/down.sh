#!/usr/bin/env bash
# Stops this checkout's verify instance: the vite dev server and Postgres container up.sh started, and its
# agent-browser session (KEEP_BROWSER=1 keeps it). Keeps dashboard/.verify/evidence.
# Usage: scripts/verify/down.sh
set -uo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
name=ployz-verify-$(printf %s "$(dirname "$dash")" | sha1sum | cut -c1-10)
run=$dash/.verify/run
if [ -f "$run/vite.pid" ]; then
  pid=$(cat "$run/vite.pid")
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
  for _ in $(seq 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.25; done
  kill -KILL -- "-$pid" 2>/dev/null || true
fi
docker rm --force "$name" >/dev/null 2>&1 || true
[ "${KEEP_BROWSER:-0}" = 1 ] || agent-browser --session "$name" close >/dev/null 2>&1 || true
rm -rf "$run"
echo "down: $name (evidence kept in $dash/.verify/evidence)"
