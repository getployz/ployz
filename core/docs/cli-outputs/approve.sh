# shellcheck shell=bash
# Sourced by the drivers and approve.test.sh; needs P (the ployz to run) and cap (cap.sh).

# approved NAME FILE TITLE -- CMD...   a pipe capture of a command Cloud asks a
# person to approve. The one approval the command names in its own
# `next: ployz org approval approve <id>` line is approved on a terminal, typing
# NAME, the Organization's name that command asks for. The run stops before .ok
# when the command names no single id within 120s, the approver doesn't finish
# within APPROVE_TIMEOUT seconds (60), or the command then exits non-zero. The
# command's stderr must reach this shell as it is written.
approved() {
  local name=$1 err id='' cap_pid
  shift
  err=$(mktemp) || exit 1
  CAP_TTY=0 CAP_MUST=1 CAP_STDERR=$err setsid "${cap:?}" "$@" &
  cap_pid=$!
  for _ in $(seq 120); do
    id=$(sed -n 's/^next: ployz org approval approve \([0-9A-Za-z-]*\)$/\1/p' "$err" | sort -u)
    [ -z "$id" ] || break
    kill -0 "$cap_pid" 2>/dev/null || break
    sleep 1
  done
  if [ -n "$id" ] && [ "$(wc -l <<<"$id")" = 1 ] &&
    printf '%s\n' "$name" | timeout "${APPROVE_TIMEOUT:-60}" script -qec "$(printf '%q ' "${P:?}" org approval approve "$id")" /dev/null >/dev/null 2>&1 &&
    wait "$cap_pid"; then
    rm -f "$err"
    return
  fi
  kill -- -"$cap_pid" 2>/dev/null
  wait "$cap_pid" 2>/dev/null
  rm -f "$err"
  echo "approved: ${id:-no approval id} for '$3' was not approved, or the command then failed" >&2
  exit 1
}
