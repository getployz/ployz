#!/usr/bin/env bash
# Runs down.sh once nothing has been connected to this checkout's vite port for VERIFY_IDLE_TIMEOUT seconds
# (default 900). An open page holds vite's HMR socket, so a live browser keeps the instance up.
# Started by up.sh; VERIFY_IDLE_TIMEOUT=0 turns it off. Usage: scripts/verify/idle.sh <port>
set -uo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
port=$1 timeout=${VERIFY_IDLE_TIMEOUT:-900} step=${VERIFY_IDLE_STEP:-30}
connected() {
  if command -v ss >/dev/null; then [ -n "$(ss -Htn state established "( sport = :$port )")" ]
  else lsof -nP -iTCP:"$port" -sTCP:ESTABLISHED -t >/dev/null 2>&1; fi
}
idle=0
while [ "$idle" -lt "$timeout" ]; do
  sleep "$step"
  [ -d "$dash/.verify/run" ] || exit 0
  if connected; then idle=0; else idle=$((idle + step)); fi
done
echo "idle: no connections to :$port for ${timeout}s, stopping"
# Own process group, so down.sh stopping this script's group does not stop down.sh.
exec setsid "$dash/scripts/verify/down.sh"
