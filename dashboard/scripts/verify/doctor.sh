#!/usr/bin/env bash
# Read-only health check of this checkout's verify instance. Prints one line per check; exits 1 if any fail.
# Usage: scripts/verify/doctor.sh
set -uo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
wt=$(dirname "$dash")
name=ployz-verify-$(printf %s "$wt" | sha1sum | cut -c1-10)
run=$dash/.verify/run
fail=0
check() { if eval "$2" >/dev/null 2>&1; then echo "ok    $1"; else echo "FAIL  $1${3:+ -> $3}"; fail=1; fi; }

check "node_modules installed" "[ -f '$dash/node_modules/.modules.yaml' ]" "up.sh installs it"
check "native SDK built" "[ -f '$wt/core/crates/ployz-sdk/ployz-sdk.node' ]" "up.sh builds it"
check "instance started" "[ -f '$run/seed.json' ] && [ -f '$run/env' ]" "run scripts/verify/up.sh"
check "postgres container up" "[ \"\$(docker inspect -f '{{.State.Running}}' $name)\" = true ]" "run scripts/verify/up.sh"
check "vite running" "kill -0 \$(cat '$run/vite.pid')" "see $run/vite.log"
if [ -f "$run/env" ]; then
  port=$(sed -n 's/^PORT=//p' "$run/env")
  cookie=$(node -e 'console.log(require(process.argv[1]).cookie)' "$run/seed.json" 2>/dev/null)
  org=$(node -e 'console.log(require(process.argv[1]).organizationSlug)' "$run/seed.json" 2>/dev/null)
  where=$(curl -s -o /dev/null -w '%{redirect_url}' -H "Cookie: better-auth.session_token=$cookie" "http://127.0.0.1:$port/cloud")
  check "signed in on :$port (/cloud -> /cloud/$org)" "case '$where' in */cloud/$org*) true ;; *) false ;; esac" "got '$where'"
fi
exit $fail
