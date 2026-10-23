#!/usr/bin/env bash
# Removes the verify containers no instance uses: one whose checkout is gone, or whose checkout's vite has stopped,
# as when its idle watchdog was killed or its run ended without down.sh. A live instance whose watchdog died gets
# its watchdog back, with the timeout its up.sh chose. Only containers up.sh labelled with their checkout are
# touched, and one younger than VERIFY_REAP_GRACE seconds (default 600) is left, since its up.sh may still be
# starting vite. A checkout this reaper cannot fully see is left too: an unmounted or private directory may hide a
# live instance.
# up.sh runs it first; run it alone any time. VERIFY_REAP_DRY=1 lists what it would remove and changes nothing.
# Usage: scripts/verify/reap.sh
set -uo pipefail
grace=${VERIFY_REAP_GRACE:-600} dry=${VERIFY_REAP_DRY:-0}
# Whether pid $1 runs a command matching $2: a pid a dead instance left may since belong to another process.
runs() { [ -n "$1" ] && ps -o args= -p "$1" 2>/dev/null | grep -q -- "$2"; }
age() { node -e 'console.log(Math.floor((Date.now() - Date.parse(process.argv[1])) / 1000))' "$1"; }
# The working directory of pid $1, or nothing when this host does not say.
cwd() {
  if [ -e "/proc/$1" ]; then readlink "/proc/$1/cwd" 2>/dev/null
  else lsof -a -p "$1" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p'; fi
}
# Whose vite pid $1 is: "own" when it runs in checkout $2's dashboard, as up.sh starts it, "unknown" when it runs
# vite but its directory is unreadable, and "none" otherwise, as when another checkout's vite took the pid.
vite_of() {
  local dir
  runs "$1" vite/bin/vite.js || { echo none; return; }
  dir=$(cwd "$1")
  if [ -z "$dir" ]; then echo unknown
  elif [ "$dir" = "$(cd "$2/dashboard" && pwd -P)" ]; then echo own
  else echo none; fi
}
# Whether this reaper sees all of checkout $1: its parent exists, and it may enter every part of it that exists.
visible() {
  local path
  [ -d "$(dirname "$1")" ] || return 1
  for path in "$(dirname "$1")" "$1" "$1/dashboard" "$1/dashboard/.verify" "$1/dashboard/.verify/run"; do
    if [ -e "$path" ] && [ ! -x "$path" ]; then return 1; fi
  done
}
# Starts the idle watchdog of checkout $1 again, under the lock up.sh holds while it replaces the watchdog.
rewatch() {
  local wt=$1 run=$1/dashboard/.verify/run port timeout
  command -v setsid >/dev/null || return 0
  mkdir "$run/idle.lock" 2>/dev/null || return 0
  port=$(sed -n 's/^PORT=//p' "$run/env" 2>/dev/null)
  timeout=$(cat "$run/idle-timeout" 2>/dev/null)
  if [ -n "$port" ] && [ -n "$timeout" ] && [ "$timeout" != 0 ] && [ -f "$run/idle.pid" ] \
    && ! runs "$(cat "$run/idle.pid" 2>/dev/null)" idle.sh; then
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
  if [ -z "$wt" ] || ! visible "$wt"; then continue; fi
  [ "$(age "$created")" -ge "$grace" ] 2>/dev/null || continue
  run=$wt/dashboard/.verify/run
  if [ ! -d "$wt/dashboard" ]; then
    reason="its checkout $wt is gone"
  else
    case $(vite_of "$(cat "$run/vite.pid" 2>/dev/null)" "$wt") in
      own) if [ "$dry" != 1 ]; then rewatch "$wt"; fi; continue ;;
      unknown) continue ;;
    esac
    reason="no vite runs for $wt"
  fi
  if [ "$dry" = 1 ]; then echo "reap: would remove ${name#/} ($reason)"; continue; fi
  docker rm --force "$id" >/dev/null 2>&1 && echo "reap: removed ${name#/} ($reason)"
done
exit 0
