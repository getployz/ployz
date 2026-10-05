#!/usr/bin/env bash
# Stops this checkout's verify instance: the cluster, vite dev server, Inngest dev server and worker, fake Hosted DNS
# and Postgres container up.sh started, and its agent-browser session (KEEP_BROWSER=1 keeps it). Keeps
# dashboard/.verify/evidence and the cluster's evidence.
# Usage: scripts/verify/down.sh
set -uo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
wt=$(dirname "$dash")
name=ployz-verify-$(printf %s "$wt" | sha1sum | cut -c1-10)
run=$dash/.verify/run
[ -f "$run/cluster" ] && "$wt/core/scripts/verify-cluster" down "$(cat "$run/cluster")"
for process in worker vite inngest hosted-dns; do
  [ -f "$run/$process.pid" ] || continue
  pid=$(cat "$run/$process.pid")
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
  for _ in $(seq 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.25; done
  kill -KILL -- "-$pid" 2>/dev/null || true
done
docker rm --force "$name" >/dev/null 2>&1 || true
[ "${KEEP_BROWSER:-0}" = 1 ] || agent-browser --session "$name" close >/dev/null 2>&1 || true
rm -rf "$run"
echo "down: $name (evidence kept in $dash/.verify/evidence)"
