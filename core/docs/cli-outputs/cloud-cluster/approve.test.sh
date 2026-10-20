#!/usr/bin/env bash
# Checks approve.sh's `approved` against a stub ployz: it approves only the
# approval its own capture asked for, fails when the capture fails, and fails
# within its deadline when the approver stalls.
set -euo pipefail
HERE=$(cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cat >"$work/ployz" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
d=$STUB_DIR
case "$*" in
  "--json org approval ls")
    printf '['; sep=''
    while read -r id; do printf '%s{"id":"%s","status":"pending"}' "$sep" "$id"; sep=,; done <"$d/pending"
    echo ']' ;;
  "org approval approve "*)
    [ ! -e "$d/stall" ] || sleep 600
    read -r name
    echo "${*: -1} $name" >>"$d/approved" ;;
  "gated "*)
    # Another request lands first and sorts first; then this command's own.
    echo 00000000-other >>"$d/pending"
    echo ffffffff-own >>"$d/pending"
    echo 'This removal destroys 1 thing:' >&2
    echo 'Approve ffffffff-own in Ployz Cloud at http://localhost:1' >&2
    echo 'next: ployz org approval approve ffffffff-own' >&2
    for _ in $(seq 100); do
      if grep -q '^ffffffff-own ada$' "$d/approved" 2>/dev/null; then exit "$2"; fi
      sleep 0.1
    done
    exit 124 ;;
  *) echo "stub: unexpected $*" >&2; exit 2 ;;
esac
STUB
chmod +x "$work/ployz"

fails=0
# scenario NAME EXIT STALL: runs `approved` in a fresh stub state; prints its status and what got approved.
scenario() {
  local d=$work/$1
  mkdir -p "$d"; : >"$d/pending"; : >"$d/approved"
  [ "$3" = 0 ] || : >"$d/stall"
  local status=0
  STUB_DIR=$d P=$work/ployz cap=$HERE/../cap.sh APPROVE_TIMEOUT=3 \
    timeout 40 bash -c 'set -euo pipefail; source "$1"; approved "$2" "gated" -- "$P" gated "$3"; echo done' \
    _ "$HERE/approve.sh" "$d/out.md" "$2" >"$d/log" 2>&1 || status=$?
  echo "$status"
}
check() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1: got '$2', want '$3'"; fails=1; fi
}

s=$(scenario concurrent 0 0)
check "approves only the capture's own approval" "$(cat "$work/concurrent/approved")" "ffffffff-own ada"
check "a gated capture that succeeds passes" "$s" 0

s=$(scenario failing 124 0)
check "a gated capture that fails stops the run" "$([ "$s" != 0 ] && [ "$s" != 124 ] && echo stopped || echo "exit $s")" stopped

start=$SECONDS
s=$(scenario stalled 0 1)
check "a stalled approver stops the run" "$([ "$s" != 0 ] && echo stopped || echo "exit $s")" stopped
check "it stops before the 40s guard" "$([ "$s" != 124 ] && [ $((SECONDS - start)) -lt 35 ] && echo bounded || echo "exit $s after $((SECONDS - start))s")" bounded

exit "$fails"
