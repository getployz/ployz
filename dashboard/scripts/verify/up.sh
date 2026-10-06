#!/usr/bin/env bash
# Seeded, signed-in Ployz Cloud for this checkout: its own Postgres container and `vite dev`, side by side with
# other checkouts. Re-running restarts from a fresh seed; the cookie stays valid.
# Usage: scripts/verify/up.sh [port]      BILLING=1 turns billing on. State: dashboard/.verify/run (down.sh removes it).
# SERVERS=N [DAEMON=stable|beta|checkout|<version>] also enrolls N real Machines through it (core/scripts/verify-cluster);
# down.sh tears that cluster down too. Real Servers also get a per-checkout Inngest dev server and worker.
set -euo pipefail
dash=$(cd "$(dirname "$0")/../.." && pwd)
wt=$(dirname "$dash")
name=ployz-verify-$(printf %s "$wt" | sha1sum | cut -c1-10)
run=$dash/.verify/run
[ -n "${SERVERS:-}" ] && export VERIFY_REAL_SERVERS=1

KEEP_BROWSER=1 "$dash/scripts/verify/down.sh" >/dev/null 2>&1 || true
# Prints N distinct free ports: every listener stays open until all are allocated.
free_ports() {
  node -e 'const net=require("net");Promise.all(Array.from({length:+process.argv[1]},()=>new Promise(r=>{const s=net.createServer().listen(0,"127.0.0.1",()=>r(s))}))).then(ss=>{console.log(ss.map(s=>s.address().port).join(" "));ss.forEach(s=>s.close())})' "$1"
}
port=${1:-$(free_ports 1)}
if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then echo "up.sh: port $port is in use" >&2; exit 1; fi
mkdir -p "$run" "$dash/.verify/evidence"

# Fresh checkouts have neither node_modules nor the native SDK; rebuild the SDK when core sources are newer.
if [ ! -f "$dash/node_modules/.modules.yaml" ] || [ "$dash/pnpm-lock.yaml" -nt "$dash/node_modules/.modules.yaml" ]; then
  (cd "$dash" && pnpm install --frozen-lockfile) > "$run/install.log" 2>&1 || { cat "$run/install.log" >&2; exit 1; }
fi
sdk=$wt/core/crates/ployz-sdk/ployz-sdk.node
if [ ! -f "$sdk" ] || [ -n "$(find "$wt/core/crates" -newer "$sdk" -type f \( -name '*.rs' -o -name Cargo.toml \) -print -quit)" ]; then
  echo "up.sh: building the native SDK ($run/sdk.log)"
  bash "$wt/core/scripts/build-cloud-sdk.sh" > "$run/sdk.log" 2>&1 || { tail -20 "$run/sdk.log" >&2; exit 1; }
fi

# Postgres: named for the checkout, host port from Docker, data on tmpfs (down.sh deletes it).
docker run --detach --rm --name "$name" --label "ployz.verify.checkout=$wt" \
  --tmpfs /var/lib/postgresql/data:rw --publish 127.0.0.1::5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=ployz_cloud postgres:16-alpine \
  -c fsync=off -c synchronous_commit=off -c full_page_writes=off >/dev/null
pg_port=$(docker port "$name" 5432/tcp | head -1 | sed 's/.*://')
for _ in $(seq 120); do docker exec "$name" pg_isready -q -h 127.0.0.1 -U postgres -d ployz_cloud && break; sleep 0.5; done

# A fake Hosted DNS grants Cluster Domains, so generated https addresses deploy (log: $run/hosted-dns.log).
cd "$dash"
setsid nohup node scripts/verify/runner.mjs /scripts/verify/hosted-dns.ts > "$run/hosted-dns.log" 2>&1 < /dev/null &
echo $! > "$run/hosted-dns.pid"
hosted_dns=
for _ in $(seq 120); do
  hosted_dns=$(sed -n 's/^VERIFY_HOSTED_DNS //p' "$run/hosted-dns.log") && [ -n "$hosted_dns" ] && break; sleep 0.25
done
[ -n "$hosted_dns" ] || { echo "up.sh: the fake Hosted DNS did not start (see $run/hosted-dns.log)" >&2; cat "$run/hosted-dns.log" >&2; exit 1; }

# Secrets are stable per checkout, so the browser's cookie survives a restart. A dead URL (port 9) keeps Inngest
# off shared services without real Servers; the GitHub App and Polar are fake.
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
if [ "${VERIFY_REAL_SERVERS:-0}" = 1 ]; then
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
$polar
POLAR_SERVER=sandbox
INNGEST_EVENT_KEY=verify-event-key
INNGEST_SIGNING_KEY=signkey-verify-00
$inngest
PLOYZ_HOSTED_DNS_URL=$hosted_dns
APP_ENCRYPTION_SECRET=$secret-encryption
VERIFY_SESSION_TOKEN=$(printf 'session:%s' "$wt" | sha256sum | cut -c1-32)
EOF
set -a; . "$run/env"; set +a

cd "$dash"
node node_modules/drizzle-kit/bin.cjs migrate > "$run/migrate.log" 2>&1 || { cat "$run/migrate.log" >&2; exit 1; }
seed_out=$(node scripts/verify/runner.mjs /scripts/verify/seed.ts 2>&1) || { echo "$seed_out" >&2; exit 1; }
seed=$(printf '%s\n' "$seed_out" | sed -n 's/^VERIFY_SEED //p')
printf '%s\n' "$seed" > "$run/seed.json"; echo "$name" > "$run/session"
json() { node -e 'console.log(JSON.parse(process.argv[1])[process.argv[2]])' "$seed" "$1"; }
org=$(json organizationSlug); cookie=$(json cookie)

if [ "${VERIFY_REAL_SERVERS:-0}" = 1 ]; then
  # Connect handshakes fail when the dev server inherits an HTTP(S) proxy; it needs no egress.
  env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy setsid nohup node_modules/inngest-cli/bin/inngest dev --no-discovery --no-poll --port "$inngest_port" \
    --connect-gateway-port "$gateway_port" --connect-gateway-grpc-port "$gateway_grpc_port" \
    --connect-executor-grpc-port "$executor_grpc_port" > "$run/inngest.log" 2>&1 < /dev/null &
  echo $! > "$run/inngest.pid"
  # Built from this checkout every run (about a second), so the worker never runs stale functions.
  node node_modules/vite/bin/vite.js build --config vite.worker.config.ts --outDir "$run/worker" --emptyOutDir \
    > "$run/worker-build.log" 2>&1 || { cat "$run/worker-build.log" >&2; exit 1; }
  for _ in $(seq 60); do curl -fs -o /dev/null "http://127.0.0.1:$inngest_port" && break; sleep 0.5; done
  echo "$worker_port" > "$run/worker.port"
  "$dash/scripts/verify/worker.sh"
fi

setsid nohup node node_modules/vite/bin/vite.js dev --port "$port" --strictPort --host 127.0.0.1 > "$run/vite.log" 2>&1 < /dev/null &
echo $! > "$run/vite.pid"
for _ in $(seq 240); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -H "Cookie: better-auth.session_token=$cookie" "http://127.0.0.1:$port/cloud" || true)
  [ "$code" != 000 ] && break; sleep 0.5
done
# Signed in: /cloud redirects into the organization, not to /auth. Also warms the SSR bundle.
where=$(curl -s -o /dev/null -w '%{redirect_url}' -H "Cookie: better-auth.session_token=$cookie" "http://127.0.0.1:$port/cloud")
case "$where" in */cloud/$org*) ;; *) echo "up.sh: session check failed: /cloud -> '$where' (see $run/vite.log)" >&2; exit 1 ;; esac
base=http://localhost:$port
curl -s -o /dev/null -H "Cookie: better-auth.session_token=$cookie" "$base/cloud/$org/shop/production" || true

cat <<EOF | tee "$run/info"
Ployz Cloud verify: $base  (billing $([ "${BILLING:-0}" = 1 ] && echo on || echo off))
  session $name, postgres 127.0.0.1:$pg_port, hosted DNS $hosted_dns, log $run/vite.log, evidence $dash/.verify/evidence, stop: scripts/verify/down.sh
  projects    $base/cloud/$org/~
  production  $base/cloud/$org/shop/production
  branch      $base/cloud/$org/shop/fix-api
  servers     $base/cloud/$org/~/servers
browser:
  agent-browser --session $name cookies set better-auth.session_token '$cookie' --url $base --httpOnly --sameSite Lax
  agent-browser --session $name open $base/cloud/$org/shop/production
EOF
if [ "${VERIFY_REAL_SERVERS:-0}" = 1 ]; then
  echo "inngest: http://127.0.0.1:$inngest_port (function runs; logs $run/inngest.log, $run/worker.log)" | tee -a "$run/info"
fi

if [ -n "${SERVERS:-}" ]; then
  echo "up.sh: enrolling $SERVERS Machines through Cloud (DAEMON=${DAEMON:-stable})"
  cluster=$("$wt/core/scripts/verify-cluster" up --machines "$SERVERS" --daemon "${DAEMON:-stable}" --cloud-attach)
  echo "$cluster" > "$run/cluster"
  cat <<EOF | tee -a "$run/info"
cluster: $cluster
  $wt/core/scripts/verify-cluster cli $cluster -- --json server ls
EOF
fi
