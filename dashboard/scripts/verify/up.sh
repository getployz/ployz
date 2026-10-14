#!/usr/bin/env bash
# Seeded, signed-in Ployz Cloud for this checkout: its own Postgres container and `vite dev`, side by side with
# other checkouts. Re-running in the same mode reseeds in place and keeps vite and Postgres running; --fresh, a new
# mode, a new port, new dependencies, an edited up.sh or a dead Hosted DNS restart both. The cookie stays valid.
# Usage: scripts/verify/up.sh [--fresh] [port]      BILLING=1 turns billing on. State: dashboard/.verify/run (down.sh removes it).
# SERVERS=N [DAEMON=...] also enrolls N real Machines through it (core/scripts/verify-cluster);
# DAEMON is space-separated [machine-N=]stable|beta|checkout|<version>, e.g. DAEMON="checkout machine-2=0.2.3";
# down.sh tears that cluster down too. Real Servers also get a per-checkout Inngest dev server and worker.
# FAKE_GITHUB=1 points Cloud at a fake GitHub (fake-github.ts, fixtures .verify/github.json) and starts Inngest too.
# Stops itself after 15 idle minutes with no connection to vite (idle.sh; VERIFY_IDLE_TIMEOUT=<seconds>, 0 keeps it up).
set -euo pipefail
# A verify session takes main's SDK build while the SDK's interface is unchanged; SDK=exact builds this checkout's.
export SDK=${SDK:-stale}
dash=$(cd "$(dirname "$0")/../.." && pwd)
wt=$(dirname "$dash")
name=ployz-verify-$(printf %s "$wt" | sha1sum | cut -c1-10)
run=$dash/.verify/run
fresh=0
if [ "${1:-}" = --fresh ]; then fresh=1; shift; fi
[ -n "${SERVERS:-}" ] && export VERIFY_REAL_SERVERS=1
if [ "${VERIFY_REAL_SERVERS:-0}" = 1 ] || [ "${FAKE_GITHUB:-0}" = 1 ]; then with_inngest=1; else with_inngest=0; fi
cd "$dash"

# Fresh checkouts have neither node_modules nor the native SDK; rebuild the SDK when core sources are newer.
install=0; build_sdk=0
if [ ! -f "$dash/node_modules/.modules.yaml" ] || [ "$dash/pnpm-lock.yaml" -nt "$dash/node_modules/.modules.yaml" ]; then install=1; fi
sdk=$wt/core/crates/ployz-sdk/ployz-sdk.node
if [ ! -f "$sdk" ] || [ -n "$(find "$wt/core/crates" -newer "$sdk" -type f \( -name '*.rs' -o -name Cargo.toml \) -print -quit)" ]; then build_sdk=1; fi

# vite reads the env file once and loads the SDK once, and the worker runs a bundle of the code it started with, so
# only an instance without Inngest, started by this up.sh in this mode on this port, with unchanged dependencies,
# is reused.
mode="billing=${BILLING:-0} fake-servers=${FAKE_SERVERS:-0} fake-github=${FAKE_GITHUB:-0} up=$(sha1sum < "$dash/scripts/verify/up.sh" | cut -c1-12)"
reseed=0
if [ "$fresh" = 0 ] && [ "$with_inngest" != 1 ] && [ "$install$build_sdk" = 00 ] \
  && [ "$(cat "$run/mode" 2>/dev/null)" = "$mode" ] && kill -0 "$(cat "$run/vite.pid" 2>/dev/null)" 2>/dev/null \
  && kill -0 "$(cat "$run/hosted-dns.pid" 2>/dev/null)" 2>/dev/null \
  && [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null)" = true ] \
  && { [ -z "${1:-}" ] || [ "$1" = "$(sed -n 's/^PORT=//p' "$run/env")" ]; }; then
  reseed=1
fi

if [ "$reseed" = 1 ]; then
  echo "up.sh: reseeding the running instance (--fresh restarts it)"
  set -a; . "$run/env"; set +a
  port=$PORT; hosted_dns=$PLOYZ_HOSTED_DNS_URL
  pg_port=$(docker port "$name" 5432/tcp | head -1 | sed 's/.*://')
  # The last uptime seed may still be writing; it must be gone before its signal is cleared.
  if [ -f "$run/seed-uptime.pid" ]; then
    seeding=$(cat "$run/seed-uptime.pid")
    kill -TERM -- "-$seeding" 2>/dev/null || true
    while kill -0 "$seeding" 2>/dev/null; do sleep 0.1; done
  fi
  rm -f "$run/uptime" "$run/seed.json" "$run/info"
  # TRUNCATE keeps vite's pooled connections valid, where DROP DATABASE leaves the next page a 500. The Config
  # Store's migration ledger stays, as drizzle's does in its own schema, so neither reapplies its migrations.
  docker exec "$name" psql -q -v ON_ERROR_STOP=1 -U postgres -d ployz_cloud -c "DO \$\$ BEGIN EXECUTE (SELECT
    'TRUNCATE ' || string_agg(format('%I.%I', schemaname, tablename), ', ') || ' RESTART IDENTITY CASCADE'
    FROM pg_tables WHERE schemaname = 'public' AND tablename <> 'config_migration'); END \$\$" > "$run/truncate.log" 2>&1 || { cat "$run/truncate.log" >&2; exit 1; }
else
  KEEP_BROWSER=1 "$dash/scripts/verify/down.sh" >/dev/null 2>&1 || true
  # Prints N distinct free ports: every listener stays open until all are allocated.
  free_ports() {
    node -e 'const net=require("net");Promise.all(Array.from({length:+process.argv[1]},()=>new Promise(r=>{const s=net.createServer().listen(0,"127.0.0.1",()=>r(s))}))).then(ss=>{console.log(ss.map(s=>s.address().port).join(" "));ss.forEach(s=>s.close())})' "$1"
  }
  port=${1:-$(free_ports 1)}
  if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then echo "up.sh: port $port is in use" >&2; exit 1; fi
  mkdir -p "$run" "$dash/.verify/evidence"
  echo "$mode" > "$run/mode"

  if [ "$install" = 1 ]; then
    pnpm install --frozen-lockfile > "$run/install.log" 2>&1 || { cat "$run/install.log" >&2; exit 1; }
  fi
  if [ "$build_sdk" = 1 ]; then
    echo "up.sh: building the native SDK ($run/sdk.log)"
    bash "$wt/core/scripts/build-cloud-sdk.sh" > "$run/sdk.log" 2>&1 || { tail -20 "$run/sdk.log" >&2; exit 1; }
    # A cached SDK keeps its old mtime, older than a fresh checkout's sources; without this every rerun rebuilds.
    touch "$sdk"
  fi

  # A fake Hosted DNS grants Cluster Domains, so generated https addresses deploy (log: $run/hosted-dns.log).
  setsid nohup node scripts/verify/runner.mjs /scripts/verify/hosted-dns.ts > "$run/hosted-dns.log" 2>&1 < /dev/null &
  echo $! > "$run/hosted-dns.pid"
  if [ "${FAKE_GITHUB:-0}" = 1 ]; then
    FAKE_GITHUB_CLOUD_URL=http://127.0.0.1:$port GITHUB_APP_WEBHOOK_SECRET=verify-webhook-secret \
      setsid nohup node scripts/verify/runner.mjs /scripts/verify/fake-github.ts > "$run/fake-github.log" 2>&1 < /dev/null &
    echo $! > "$run/fake-github.pid"
  fi

  # Postgres: named for the checkout, host port from Docker, data on tmpfs (down.sh deletes it).
  docker run --detach --rm --name "$name" --label "ployz.verify.checkout=$wt" \
    --tmpfs /var/lib/postgresql/data:rw --publish 127.0.0.1::5432 \
    -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=ployz_cloud postgres:16-alpine \
    -c fsync=off -c synchronous_commit=off -c full_page_writes=off >/dev/null
  pg_port=$(docker port "$name" 5432/tcp | head -1 | sed 's/.*://')

  hosted_dns=
  for _ in $(seq 120); do
    hosted_dns=$(sed -n 's/^VERIFY_HOSTED_DNS //p' "$run/hosted-dns.log") && [ -n "$hosted_dns" ] && break; sleep 0.25
  done
  [ -n "$hosted_dns" ] || { echo "up.sh: the fake Hosted DNS did not start (see $run/hosted-dns.log)" >&2; cat "$run/hosted-dns.log" >&2; exit 1; }
  github_origin=
  if [ "${FAKE_GITHUB:-0}" = 1 ]; then
    for _ in $(seq 120); do
      fake_github=$(sed -n 's/^VERIFY_FAKE_GITHUB //p' "$run/fake-github.log") && [ -n "$fake_github" ] && break; sleep 0.25
    done
    [ -n "$fake_github" ] || { echo "up.sh: the fake GitHub did not start (see $run/fake-github.log)" >&2; cat "$run/fake-github.log" >&2; exit 1; }
    github_origin="PLOYZ_GITHUB_ORIGIN=$fake_github"
  fi

  # Secrets are stable per checkout, so the browser's cookie survives a restart. A dead URL (port 9) keeps Inngest
  # off shared services without real Servers or FAKE_GITHUB; the GitHub App and Polar are fake.
  secret=ployz-verify-$(printf 'secret:%s' "$wt" | sha256sum | cut -c1-24)
  key=$(node -e 'const {generateKeyPairSync:g}=require("crypto");process.stdout.write(g("rsa",{modulusLength:2048}).privateKey.export({type:"pkcs8",format:"pem"}).replace(/\n/g,"\\n"))')
  if [ "${BILLING:-0}" = 1 ]; then
    polar="POLAR_ACCESS_TOKEN=verify-polar-token
POLAR_WEBHOOK_SECRET=verify-polar-webhook
POLAR_PRODUCT_ID=06cdcd79-3dcd-46a5-939e-8784efa55034"
  else
    polar="POLAR_ACCESS_TOKEN=
POLAR_WEBHOOK_SECRET=
POLAR_PRODUCT_ID="
  fi
  if [ "$with_inngest" = 1 ]; then
    read -r inngest_port gateway_port gateway_grpc_port executor_grpc_port worker_port <<< "$(free_ports 5)"
    inngest="INNGEST_DEV=1
INNGEST_BASE_URL=http://127.0.0.1:$inngest_port
INNGEST_EVENT_API_BASE_URL=http://127.0.0.1:$inngest_port
INNGEST_CONNECT_GATEWAY_URL=ws://127.0.0.1:$gateway_port/v0/connect"
  else
    inngest="INNGEST_DEV=0
INNGEST_BASE_URL=http://127.0.0.1:9
INNGEST_EVENT_API_BASE_URL=http://127.0.0.1:9"
  fi
  # Without real Servers, metric reads answer from src/modules/metrics/metrics.verify-fixture.ts.
  if [ -n "${SERVERS:-}" ]; then verify_metrics=0; else verify_metrics=1; fi
  if [ -z "${SERVERS:-}" ] && [ "${FAKE_SERVERS:-}" = 1 ]; then verify_runtime=1; else verify_runtime=0; fi
  cat > "$run/env" <<EOF
NODE_ENV=development
PORT=$port
APP_URL=http://localhost:$port
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:$pg_port/ployz_cloud
BETTER_AUTH_SECRET=$secret
BETTER_AUTH_TRUSTED_ORIGINS=
GITHUB_CLIENT_ID=verify-oauth-client
GITHUB_CLIENT_SECRET=verify-oauth-secret
GITHUB_APP_ID=1
GITHUB_APP_SLUG=ployz-verify
GITHUB_APP_WEBHOOK_SECRET=verify-webhook-secret
GITHUB_APP_PRIVATE_KEY='$key'
$github_origin
$polar
POLAR_SERVER=sandbox
INNGEST_EVENT_KEY=verify-event-key
INNGEST_SIGNING_KEY=signkey-verify-00
$inngest
PLOYZ_HOSTED_DNS_URL=$hosted_dns
APP_ENCRYPTION_SECRET=$secret-encryption
UPTIME_ENABLED=true
PLOYZ_VERIFY_METRICS=$verify_metrics
PLOYZ_VERIFY_RUNTIME=$verify_runtime
VERIFY_SESSION_TOKEN=$(printf 'session:%s' "$wt" | sha256sum | cut -c1-32)
EOF
  set -a; . "$run/env"; set +a

  # vite needs only the env file, so it starts now. A signed-out request starts its SSR transform, the slowest step,
  # while Postgres is migrated and seeded; the signed-in check below then finds most of it compiled.
  setsid nohup node node_modules/vite/bin/vite.js dev --port "$port" --strictPort --host 127.0.0.1 > "$run/vite.log" 2>&1 < /dev/null &
  echo $! > "$run/vite.pid"
  (for _ in $(seq 120); do curl -s -o /dev/null "http://127.0.0.1:$port/cloud" && break; sleep 0.5; done) > /dev/null 2>&1 &

  if [ "$with_inngest" = 1 ]; then
    # Connect handshakes fail when the dev server inherits an HTTP(S) proxy; it needs no egress.
    env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy setsid nohup node_modules/inngest-cli/bin/inngest dev --no-discovery --no-poll --port "$inngest_port" \
      --connect-gateway-port "$gateway_port" --connect-gateway-grpc-port "$gateway_grpc_port" \
      --connect-executor-grpc-port "$executor_grpc_port" > "$run/inngest.log" 2>&1 < /dev/null &
    echo $! > "$run/inngest.pid"
    # Built from this checkout every run, so the worker never runs stale functions.
    node node_modules/vite/bin/vite.js build --config vite.worker.config.ts --outDir "$run/worker" --emptyOutDir \
      > "$run/worker-build.log" 2>&1 &
    worker_build=$!
  fi
fi

for _ in $(seq 120); do docker exec "$name" pg_isready -q -h 127.0.0.1 -U postgres -d ployz_cloud && break; sleep 0.5; done
node node_modules/drizzle-kit/bin.cjs migrate > "$run/migrate.log" 2>&1 || { cat "$run/migrate.log" >&2; exit 1; }
seed_out=$(node scripts/verify/runner.mjs /scripts/verify/seed.ts 2>&1) || { echo "$seed_out" >&2; exit 1; }
seed=$(printf '%s\n' "$seed_out" | sed -n 's/^VERIFY_SEED //p')
# Only the Monitoring tab reads uptime, so it seeds in the background; $run/uptime says ok or failed when it ends.
setsid nohup bash -c 'node scripts/verify/runner.mjs /scripts/verify/seed-uptime.ts > "$1/seed-uptime.log" 2>&1 \
  && echo ok > "$1/uptime" || echo failed > "$1/uptime"' seed-uptime "$run" < /dev/null > /dev/null 2>&1 &
echo $! > "$run/seed-uptime.pid"
printf '%s\n' "$seed" > "$run/seed.json"; echo "$name" > "$run/session"
json() { node -e 'console.log(JSON.parse(process.argv[1])[process.argv[2]])' "$seed" "$1"; }
org=$(json organizationSlug); cookie=$(json cookie)

if [ "$with_inngest" = 1 ]; then
  wait "$worker_build" || { cat "$run/worker-build.log" >&2; exit 1; }
  for _ in $(seq 60); do curl -fs -o /dev/null "http://127.0.0.1:$inngest_port" && break; sleep 0.5; done
  echo "$worker_port" > "$run/worker.port"
  "$dash/scripts/verify/worker.sh"
fi

# Signed in: /cloud redirects into the organization, not to /auth. A cold start answers without a redirect while
# vite still optimizes dependencies, so wait for one. A deadline, since one request can block for a minute.
where=
deadline=$((SECONDS + 180))
while [ "$SECONDS" -lt "$deadline" ]; do
  where=$(curl -s -o /dev/null -w '%{redirect_url}' -H "Cookie: better-auth.session_token=$cookie" "http://127.0.0.1:$port/cloud" || true)
  [ -n "$where" ] && break; sleep 0.5
done
case "$where" in */cloud/$org*) ;; *) echo "up.sh: session check failed: /cloud -> '$where' (see $run/vite.log)" >&2; exit 1 ;; esac
if [ -f "$run/idle.pid" ]; then kill -TERM -- "-$(cat "$run/idle.pid")" 2>/dev/null || true; rm -f "$run/idle.pid"; fi
if [ "${VERIFY_IDLE_TIMEOUT:-900}" != 0 ]; then
  setsid nohup "$dash/scripts/verify/idle.sh" "$port" > "$run/idle.log" 2>&1 < /dev/null &
  echo $! > "$run/idle.pid"
fi
base=http://localhost:$port
curl -s -o /dev/null -H "Cookie: better-auth.session_token=$cookie" "$base/cloud/$org/shop/production" || true
grep -sh 'stale main@' "$run/sdk.log" >&2 || true

cat <<EOF | tee "$run/info"
Ployz Cloud verify: $base  (billing $([ "${BILLING:-0}" = 1 ] && echo on || echo off))
  session $name, postgres 127.0.0.1:$pg_port, hosted DNS $hosted_dns, log $run/vite.log, evidence $dash/.verify/evidence, stop: scripts/verify/down.sh
  projects    $base/cloud/$org/~
  production  $base/cloud/$org/shop/production
  branch      $base/cloud/$org/shop/fix-api
  servers     $base/cloud/$org/~/servers
uptime: seeding in the background for the Monitoring tab; wait with: until [ -s $run/uptime ]; do sleep 1; done; cat $run/uptime
browser:
  agent-browser --session $name cookies set better-auth.session_token '$cookie' --url $base --httpOnly --sameSite Lax
  agent-browser --session $name open $base/cloud/$org/shop/production
EOF
if [ "$with_inngest" = 1 ]; then
  echo "inngest: http://127.0.0.1:$inngest_port (function runs; logs $run/inngest.log, $run/worker.log)" | tee -a "$run/info"
fi
if [ "${FAKE_GITHUB:-0}" = 1 ]; then
  cat <<EOF | tee -a "$run/info"
fake github on $fake_github (requests $run/fake-github.log, fixtures $dash/.verify/github.json)
  webhook: curl -X POST $fake_github/_fake/webhook -d '{"event":"installation_repositories","action":"added","installation":7,"repositories":["acme/docs"]}'
EOF
fi

if [ -n "${SERVERS:-}" ]; then
  echo "up.sh: enrolling $SERVERS Machines through Cloud (DAEMON=${DAEMON:-stable})"
  daemons=()
  for daemon in ${DAEMON:-stable}; do daemons+=(--daemon "$daemon"); done
  cluster=$("$wt/core/scripts/verify-cluster" up --machines "$SERVERS" "${daemons[@]}" --cloud-attach)
  echo "$cluster" > "$run/cluster"
  cat <<EOF | tee -a "$run/info"
cluster: $cluster
  $wt/core/scripts/verify-cluster cli $cluster -- --json server ls
EOF
fi
