#!/usr/bin/env bash
# Runs down.sh once nothing has been connected to this checkout's vite port for VERIFY_IDLE_TIMEOUT seconds
# (default 900). An open page holds vite's HMR socket, so a live browser keeps the instance up.
# Started by up.sh; VERIFY_IDLE_TIMEOUT=0 turns it off. Usage: scripts/verify/idle.sh <port>
# Each step it reads the timeout up.sh recorded last, and stops once idle.pid names another watchdog: up.sh or
# reap.sh may replace it at any moment, and only the watchdog idle.pid names, with that timeout, may stop the instance.
set -uo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
run=$dash/.verify/run
port=$1 step=${VERIFY_IDLE_STEP:-30}
recorded() { cat "$run/idle-timeout" 2>/dev/null || echo "${VERIFY_IDLE_TIMEOUT:-900}"; }
mine() { [ "$(cat "$run/idle.pid" 2>/dev/null)" = "$$" ]; }
connected() {
  if command -v ss >/dev/null; then [ -n "$(ss -Htn state established "( sport = :$port )")" ]
  else lsof -nP -iTCP:"$port" -sTCP:ESTABLISHED -t >/dev/null 2>&1; fi
}
idle=0
while :; do
  sleep "$step"
  if [ ! -d "$run" ] || ! mine; then exit 0; fi
  limit=$(recorded)
  # 0 keeps the instance up; anything but a number is no timeout this watchdog may act on.
  case $limit in '' | 0 | *[!0-9]*) exit 0 ;; esac
  if connected; then idle=0; else idle=$((idle + step)); fi
  [ "$idle" -lt "$limit" ] || break
done
echo "idle: no connections to :$port for ${limit}s, stopping"
# Own process group, so down.sh stopping this script's group does not stop down.sh.
exec setsid "$dash/scripts/verify/down.sh"
