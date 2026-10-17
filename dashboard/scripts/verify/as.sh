#!/usr/bin/env bash
# Signs this checkout's agent-browser session in as one of the seeded people of organization ada, with the session
# cookie seed.ts wrote for them, and opens /cloud. A reseed keeps every cookie valid.
# Usage: scripts/verify/as.sh ada|grace|linus      ada owns it, grace is an admin, linus a member.
set -euo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
run=$dash/.verify/run
case "${1:-}" in
  ada) key=cookie ;;
  grace | linus) key=${1}Cookie ;;
  *) echo "usage: scripts/verify/as.sh ada|grace|linus" >&2; exit 2 ;;
esac
[ -s "$run/seed.json" ] || { echo "as.sh: no seed in $run; run scripts/verify/up.sh first" >&2; exit 1; }
cookie=$(node -e 'const v = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))[process.argv[2]]; if (!v) process.exit(1); console.log(v)' "$run/seed.json" "$key") \
  || { echo "as.sh: seed.json has no $key; rerun scripts/verify/up.sh" >&2; exit 1; }
base=http://localhost:$(sed -n 's/^PORT=//p' "$run/env")
session=$(cat "$run/session")
agent-browser --session "$session" cookies set better-auth.session_token "$cookie" --url "$base" --httpOnly --sameSite Lax
agent-browser --session "$session" open "$base/cloud"
