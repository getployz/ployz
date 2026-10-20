#!/usr/bin/env bash
# Checks scrub.sh against lines it must and must not rewrite: bash scrub.test.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
check() {
  printf '%s\n' "$1" >"$dir/line"
  bash "$here/scrub.sh" "$dir/line"
  local got
  got=$(cat "$dir/line")
  [ "$got" = "$2" ] || { printf 'scrub.sh turned\n  %s\ninto\n  %s\nwant\n  %s\n' "$1" "$got" "$2" >&2; exit 1; }
}
check 'public 2a01:4f8:c17:1234::1 on alpha' 'public 2001:db8::10 on alpha'
check 'ip 2a01:4f8:c17:1234:0:0:0:1.' 'ip 2001:db8::10.'
check 'mapped 2a01::ffff:192.0.2.1 stays one address' 'mapped 2001:db8::10 stays one address'
check 'example_2abc::xyz' 'example_2abc::xyz'
check '172.18.0.1 - - [20/Oct/2026:16:56:24 +0000] "GET / HTTP/1.1"' '172.18.0.1 - - [20/Oct/2026:16:56:24 +0000] "GET / HTTP/1.1"'
check 'documentation 2001:db8::5 stays' 'documentation 2001:db8::5 stays'
check 'ULA fd00::1 stays' 'ULA fd00::1 stays'
check 'host 8.8.8.8' 'host 203.0.113.10'
echo ok
