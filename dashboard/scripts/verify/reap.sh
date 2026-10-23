#!/usr/bin/env bash
# Removes the verify containers no instance uses: one whose checkout is gone, or whose checkout's vite has stopped,
# as when its idle watchdog was killed or its run ended without down.sh. A live instance whose watchdog died gets
# its watchdog back. Only containers up.sh labelled with their checkout are touched, and one younger than
# VERIFY_REAP_GRACE seconds (default 600) is left, since its up.sh may still be starting vite.
# up.sh runs it first; run it alone any time. VERIFY_REAP_DRY=1 lists what it would remove and changes nothing.
# Usage: scripts/verify/reap.sh
set -uo pipefail
grace=${VERIFY_REAP_GRACE:-600} dry=${VERIFY_REAP_DRY:-0}
alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
age() { node -e 'console.log(Math.floor((Date.now() - Date.parse(process.argv[1])) / 1000))' "$1"; }
for container in $(docker ps -a --filter label=ployz.verify.checkout --format '{{.Names}}'); do
  wt=$(docker inspect -f '{{index .Config.Labels "ployz.verify.checkout"}}' "$container" 2>/dev/null) || continue
  [ -n "$wt" ] || continue
  run=$wt/dashboard/.verify/run
  if [ ! -d "$wt/dashboard" ]; then
    reason="its checkout $wt is gone"
  else
    created=$(docker inspect -f '{{.Created}}' "$container" 2>/dev/null) || continue
    [ "$(age "$created")" -ge "$grace" ] || continue
    if alive "$(cat "$run/vite.pid" 2>/dev/null)"; then
      if [ "$dry" != 1 ] && [ -f "$run/idle.pid" ] && ! alive "$(cat "$run/idle.pid")"; then
        port=$(sed -n 's/^PORT=//p' "$run/env" 2>/dev/null)
        if [ -n "$port" ]; then
          setsid nohup "$wt/dashboard/scripts/verify/idle.sh" "$port" >> "$run/idle.log" 2>&1 < /dev/null &
          echo $! > "$run/idle.pid"
          echo "reap: started the idle watchdog of $wt again"
        fi
      fi
      continue
    fi
    reason="no vite runs for $wt"
  fi
  if [ "$dry" = 1 ]; then echo "reap: would remove $container ($reason)"; continue; fi
  docker rm --force "$container" >/dev/null 2>&1 && echo "reap: removed $container ($reason)"
done
exit 0
