#!/usr/bin/env bash
# Removes the verify containers no instance uses: one whose checkout is gone, or whose checkout's vite has stopped,
# as when its idle watchdog was killed or its run ended without down.sh. A live instance whose watchdog died gets
# its watchdog back, with the timeout its up.sh chose. Only containers up.sh labelled with their checkout are
# touched, and one younger than VERIFY_REAP_GRACE seconds (default 600) is left, since its up.sh may still be
# starting vite. A checkout whose parent directory is not visible is left too: this host may not see that mount.
# up.sh runs it first; run it alone any time. VERIFY_REAP_DRY=1 lists what it would remove and changes nothing.
# Usage: scripts/verify/reap.sh
set -uo pipefail
grace=${VERIFY_REAP_GRACE:-600} dry=${VERIFY_REAP_DRY:-0}
# Whether pid $1 runs a command matching $2: a pid a dead instance left may since belong to another process.
runs() { [ -n "$1" ] && ps -o args= -p "$1" 2>/dev/null | grep -q -- "$2"; }
age() { node -e 'console.log(Math.floor((Date.now() - Date.parse(process.argv[1])) / 1000))' "$1"; }
# Starts the idle watchdog of checkout $1 again, unless another reaper holds its lock or already did.
rewatch() {
  local wt=$1 run=$1/dashboard/.verify/run port timeout
  port=$(sed -n 's/^PORT=//p' "$run/env" 2>/dev/null)
  timeout=$(cat "$run/idle-timeout" 2>/dev/null)
  [ -n "$port" ] && [ -n "$timeout" ] && [ "$timeout" != 0 ] && command -v setsid >/dev/null || return 0
  mkdir "$run/idle.lock" 2>/dev/null || return 0
  if ! runs "$(cat "$run/idle.pid" 2>/dev/null)" idle.sh; then
    VERIFY_IDLE_TIMEOUT=$timeout setsid nohup env -u VERIFY_IDLE_STEP "$wt/dashboard/scripts/verify/idle.sh" "$port" \
      >> "$run/idle.log" 2>&1 < /dev/null &
    echo $! > "$run/idle.pid"
    echo "reap: started the idle watchdog of $wt again"
  fi
  rmdir "$run/idle.lock" 2>/dev/null
  return 0
}
# By id, never name: a checkout's up.sh may replace its container under the same name while this runs.
for id in $(docker ps -a --filter label=ployz.verify.checkout --format '{{.ID}}'); do
  wt=$(docker inspect -f '{{index .Config.Labels "ployz.verify.checkout"}}' "$id" 2>/dev/null) || continue
  name=$(docker inspect -f '{{.Name}}' "$id" 2>/dev/null) || continue
  created=$(docker inspect -f '{{.Created}}' "$id" 2>/dev/null) || continue
  if [ -z "$wt" ] || [ ! -d "$(dirname "$wt")" ]; then continue; fi
  [ "$(age "$created")" -ge "$grace" ] || continue
  run=$wt/dashboard/.verify/run
  if [ ! -d "$wt/dashboard" ]; then
    reason="its checkout $wt is gone"
  elif runs "$(cat "$run/vite.pid" 2>/dev/null)" vite/bin/vite.js; then
    if [ "$dry" != 1 ] && [ -f "$run/idle.pid" ]; then rewatch "$wt"; fi
    continue
  else
    reason="no vite runs for $wt"
  fi
  if [ "$dry" = 1 ]; then echo "reap: would remove ${name#/} ($reason)"; continue; fi
  docker rm --force "$id" >/dev/null 2>&1 && echo "reap: removed ${name#/} ($reason)"
done
exit 0
