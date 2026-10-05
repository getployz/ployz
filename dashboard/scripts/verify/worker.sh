#!/usr/bin/env bash
# Starts the Inngest worker that up.sh built into dashboard/.verify/run/worker, records its pid and waits until it
# connects. up.sh runs it first; core/scripts/verify-cluster runner start runs it again after runner kill.
set -euo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
run=$dash/.verify/run
set -a; . "$run/env"; set +a
port=$(cat "$run/worker.port")
cd "$dash"
PORT=$port setsid nohup node "$run/worker/index.mjs" >> "$run/worker.log" 2>&1 < /dev/null &
echo $! > "$run/worker.pid"
for _ in $(seq 120); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/ready")" = 200 ] && exit 0; sleep 0.5
done
echo "worker.sh: the Inngest worker did not connect (see $run/worker.log, $run/inngest.log)" >&2
exit 1
