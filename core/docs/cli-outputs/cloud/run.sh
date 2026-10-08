#!/usr/bin/env bash
# Captures every Ployz Cloud-facing CLI command against a seeded local Cloud
# (dashboard/scripts/verify/up.sh), one *.md (+ *.ansi) per group. It starts
# its own verify Cloud and refuses to run while this checkout already has one
# (dashboard/.verify/run), because up.sh would wipe it. On exit it stops only
# the Cloud it started; KEEP=1 leaves that Cloud running.
#   cloud/run.sh [PARENT]   raw captures into a fresh mktemp -d under PARENT (default /tmp)
#   publish.sh DIR       scrubs them and copies them over the tracked ones
set -euo pipefail
shopt -s inherit_errexit
# Ctrl-C stops the run before .ok, even when the command it interrupted
# (script, ployz) swallowed the signal and exited normally.
trap 'exit 130' INT
trap 'exit 143' TERM
DOCS=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
ROOT=$(cd -- "$DOCS/../../.." && pwd) || exit 1
DASH=$ROOT/dashboard
REAL=$ROOT/core/target/debug/ployz
readonly DOCS ROOT DASH REAL
if [ -e "$DASH/.verify/run" ]; then
  echo "a verify Cloud already runs for this checkout ($DASH/.verify/run); stop it with dashboard/scripts/verify/down.sh first" >&2
  exit 1
fi
CAPTURES=$("$DOCS/capture-dir.sh" "${1:-}") || exit 1
WORK=$(mktemp -d /tmp/pz-cloud-capture.XXXXXX) || exit 1
readonly CAPTURES WORK
cloud_started=0
finish() {
  if [ "$cloud_started" = 1 ] && [ "${KEEP:-0}" != 1 ]; then
    (cd "$DASH" || exit 1; scripts/verify/down.sh) >/dev/null 2>&1 || true
    echo "dashboard stopped"
  fi
  rm -rf "$WORK"
}
trap finish EXIT
echo "raw captures in $CAPTURES"
mkdir -p "$WORK/bin" "$WORK/app" "$WORK/up-app"
cap=$DOCS/cap.sh

# A wrapper so no capture hangs; cap.sh prints it as `ployz`.
cat >"$WORK/bin/ployz" <<EOF
#!/usr/bin/env bash
exec timeout --foreground "\${CAP_TIMEOUT:-30}" "$REAL" "\$@"
EOF
# Never open a real browser from `ployz login` / `github connect`.
printf '#!/bin/sh\nexit 0\n' >"$WORK/bin/xdg-open"
chmod +x "$WORK/bin/ployz" "$WORK/bin/xdg-open"
export PATH=$WORK/bin:$PATH PLOYZ_BIN=$WORK/bin/ployz
unset PLOYZ_TOKEN PLOYZ_CONTEXT PLOYZ_PROJECT PLOYZ_ENV PLOYZ_CONFIG PLOYZ_STORE PLOYZ_CONNECT XDG_CONFIG_HOME
P=$PLOYZ_BIN

c() { "$cap" "$CAPTURES/$1.md" "$2" -- "${@:3}"; }          # read-only: pipe + TTY
m() { CAP_TTY=0 "$cap" "$CAPTURES/$1.md" "$2" -- "${@:3}"; } # mutating: pipe only
need() { CAP_MUST=1 "$@"; } # a capture later captures depend on; its failure stops the run
note() { printf '%s\n\n' "$2" >>"$CAPTURES/$1.md"; }

up() {
  cloud_started=1 # anything at dashboard/.verify/run from here on is ours
  (cd "$DASH" || exit 1; scripts/verify/up.sh) >"$WORK/up.log" 2>&1 || { cat "$WORK/up.log"; exit 1; }
  U=$(sed -n 's/^Ployz Cloud verify: \([^ ]*\).*/\1/p' "$DASH/.verify/run/info")
  cookie=$(jq -r .cookie "$DASH/.verify/run/seed.json")
  export PLOYZ_CLOUD_URL=$U
  echo "Cloud at $U"
}
api() { curl -sf -b "better-auth.session_token=$cookie" -H "Origin: $U" -H 'content-type: application/json' "$@"; }
approve() {
  api "$U/api/auth/device?user_code=$1" >/dev/null || { echo "approve: device lookup failed for $1" >&2; exit 1; }
  api -d "{\"userCode\":\"$1\"}" "$U/api/auth/device/approve" >/dev/null || { echo "approve: approving $1 failed" >&2; exit 1; }
}
# Sign HOME in headlessly: start, approve as Ada, finish.
signin() {
  code=$("$REAL" login --json </dev/null | jq -r .code)
  approve "$code"
  "$REAL" login --wait </dev/null >/dev/null
}

for f in auth token org github domain deploy errors; do
  printf '# Ployz Cloud CLI outputs: %s\n\nCaptured by cloud/run.sh against a seeded local Ployz Cloud (dashboard verify), signed in as ada@example.com unless noted. Cloud URL varies per run.\n\n' "$f" >"$CAPTURES/$f.md"
done

up
cd "$WORK/app" || exit 1

# ---------- errors: not signed in, bad token, unreachable Cloud ----------
export HOME=$WORK/home-anon; mkdir -p "$HOME"
note errors "## Not signed in (fresh HOME, PLOYZ_CLOUD_URL set)"
for cmd in "status" "token ls" "org ls" "github ls" "domain ls" "deployment ls" "deploy --plan" "logs" "ps" "cloud reset -y"; do
  c errors "not signed in: $cmd" $P $cmd
  c errors "not signed in: $cmd --json" $P $cmd --json
done
c errors "logout when not signed in" $P logout
c errors "logout when not signed in --json" $P logout --json

note errors "## Unreachable Cloud"
PLOYZ_CLOUD_URL=http://127.0.0.1:9 c errors "login, Cloud unreachable" $P login
PLOYZ_CLOUD_URL=http://127.0.0.1:9 c errors "login, Cloud unreachable --json" $P login --json
PLOYZ_CLOUD_URL=http://127.0.0.1:9 PLOYZ_TOKEN=ployz_x c errors "token ls, token against unreachable Cloud" $P token ls
PLOYZ_CLOUD_URL=http://127.0.0.1:9 PLOYZ_TOKEN=ployz_x c errors "token ls, token against unreachable Cloud --json" $P token ls --json
PLOYZ_CLOUD_URL=https://example.com c errors "login --json, URL that is not a Ployz Cloud" $P login --json

note errors "## Bad PLOYZ_TOKEN"
for cmd in "status" "token ls" "org ls" "deployment ls"; do
  PLOYZ_TOKEN=ployz_bogus c errors "bogus PLOYZ_TOKEN: $cmd" $P $cmd
  PLOYZ_TOKEN=ployz_bogus c errors "bogus PLOYZ_TOKEN: $cmd --json" $P $cmd --json
done

note errors "## Usage errors (clap)"
c errors "token new without a name" $P token new
c errors "token new without a name --json" $P token new --json
c errors "org use without a slug" $P org use
c errors "unknown subcommand" $P org frobnicate
c errors "bad build-order value" $P org build-order sideways
c errors "domain add without a service" $P domain add

# ---------- auth ----------
export HOME=$WORK/home-wait; mkdir -p "$HOME"
note auth "## login from scratch, human (killed after 5s while it waits)"
CAP_TIMEOUT=5 m auth "login, waiting for approval (timed out by the capture)" $P login
CAP_TIMEOUT=5 c auth "login --wait --json, waiting (timed out by the capture)" $P login --wait --json

export HOME=$WORK/home-ada; mkdir -p "$HOME"
note auth "## login: pending, then approved"
need m auth "login --json (prints page and code at once)" $P login --json
code=$(jq -r .code "$HOME/.config/ployz/cloud.json" 2>/dev/null) || code=
[ -n "$code" ] && [ "$code" != null ] || code=$(sed -n 's/.*"code": "\([A-Z0-9]*\)".*/\1/p' "$CAPTURES/auth.md" | tail -1)
[ -n "$code" ] || { echo "login --json printed no device code" >&2; exit 1; }
c auth "login again while pending --json (resumes the same code)" $P login --json
approve "$code"
need m auth "login after approval (human, resumes the pending code)" $P login
c auth "login when already signed in" $P login
c auth "login when already signed in --json" $P login --json
note auth "## status, signed in"
c auth "status" $P status
c auth "status --json" $P status --json

# ---------- token ----------
note token "## token new / ls / rm"
need m token "token new ci" $P token new ci
need m token "token new deploy --expires-in 7 --json" $P token new deploy --expires-in 7 --json
c token "token new bad --expires-in" $P token new x --expires-in soon
c token "token ls" $P token ls
c token "token ls --json" $P token ls --json
tid=$("$REAL" token ls --json | jq -r '[.tokens[]? // empty | select(.name=="ci")][0].id // empty' 2>/dev/null) || tid=
[ -n "$tid" ] || tid=$("$REAL" token ls | awk '$3=="ci"{print $2; exit}')
did=$("$REAL" token ls | awk '$3=="deploy"{print $2; exit}')
if [ -z "$tid" ] || [ -z "$did" ]; then
  echo "token ls lists no id for the ci or deploy token" >&2
  exit 1
fi
m token "token rm ID" $P token rm "$tid"
m token "token rm ID --json" $P token rm "$did" --json
c token "token rm, already revoked ID" $P token rm "$tid"
c token "token rm, unknown ID" $P token rm 00000000-0000-0000-0000-000000000000
c token "token rm, unknown ID --json" $P token rm 00000000-0000-0000-0000-000000000000 --json
c token "token rm, not a UUID" $P token rm ci
note token "## acting with PLOYZ_TOKEN"
secret=$("$REAL" token new env-test --json | jq -r .token.secret)
revoked=$("$REAL" token new revoked --json | jq -r .token.secret)
"$REAL" token rm "$("$REAL" token ls | awk '$3=="revoked"{print $2; exit}')" >/dev/null
PLOYZ_TOKEN=$secret c token "status with PLOYZ_TOKEN" $P status
PLOYZ_TOKEN=$secret c token "status with PLOYZ_TOKEN --json" $P status --json
PLOYZ_TOKEN=$secret c token "token ls with PLOYZ_TOKEN" $P token ls
PLOYZ_TOKEN=$secret c token "org ls with PLOYZ_TOKEN" $P org ls
PLOYZ_TOKEN=$secret c token "org use with PLOYZ_TOKEN (refused)" $P org use babbage
PLOYZ_TOKEN=$secret c token "token new with PLOYZ_TOKEN" $P token new nested
PLOYZ_TOKEN=$revoked c token "status with a revoked PLOYZ_TOKEN" $P status
PLOYZ_TOKEN=$revoked c token "token ls with a revoked PLOYZ_TOKEN --json" $P token ls --json

# ---------- org ----------
api -d '{"name":"Babbage Labs","slug":"babbage","keepCurrentActiveOrganization":true}' "$U/api/auth/organization/create" >/dev/null || { echo "creating the Babbage Labs Organization failed" >&2; exit 1; }
note org "## org ls / use (second Organization babbage created through the auth API)"
c org "org ls" $P org ls
c org "org ls --json" $P org ls --json
c org "org (bare)" $P org
m org "org use babbage" $P org use babbage
need m org "org use ada --json" $P org use ada --json
c org "org use unknown" $P org use nope
c org "org use unknown --json" $P org use nope --json
note org "## org build-order"
c org "org build-order (show)" $P org build-order
c org "org build-order --json" $P org build-order --json
m org "org build-order servers-only" $P org build-order servers-only
m org "org build-order auto --json" $P org build-order auto --json
note org "## org rm"
c org "org rm ada without --confirm" $P org rm ada
c org "org rm ada --confirm wrong" $P org rm ada --confirm wrong
c org "org rm ada --confirm ada (has Projects)" $P org rm ada --confirm ada
c org "org rm ada --confirm ada --json (has Projects)" $P org rm ada --confirm ada --json
c org "org rm babbage while acting in ada" $P org rm babbage --confirm babbage
"$REAL" org use babbage >/dev/null
m org "org rm babbage --confirm babbage (empty, acting in it)" $P org rm babbage --confirm babbage
c org "org ls after removal" $P org ls
c org "status after removing the acting Organization" $P status
"$REAL" org use ada >/dev/null 2>&1 || true

# ---------- github ----------
note github "## github (GitHub App is fake in verify)"
c github "github (bare)" $P github
c github "github ls" $P github ls
c github "github ls --json" $P github ls --json
c github "github connect --json (no --wait)" $P github connect --json
CAP_TIMEOUT=6 c github "github connect (waits; timed out by the capture)" $P github connect
c github "github disconnect unknown" $P github disconnect 12345
c github "github disconnect unknown --json" $P github disconnect 12345 --json
c github "github ls REPO (unknown repository)" $P github ls acme/shop

# ---------- domain ----------
note domain "## domain (seed: acme.com on web; hosted DNS is dead in verify)"
c domain "domain ls" $P domain ls
c domain "domain ls --json" $P domain ls --json
need m domain "domain add api (generated)" $P domain add api
need m domain "domain add worker --json (generated)" $P domain add worker --json
need m domain "domain add web shop.example.org (custom)" $P domain add web shop.example.org
need m domain "domain add web docs.example.org --port 8080 --json" $P domain add web docs.example.org --port 8080 --json
c domain "domain add, duplicate host" $P domain add web shop.example.org
c domain "domain add, unknown service" $P domain add nosuch
c domain "domain add, unknown service --json" $P domain add nosuch --json
c domain "domain add, bad host" $P domain add web 'not a host'
m domain "domain set api myapi" $P domain set api myapi
m domain "domain set worker myworker --json" $P domain set worker myworker --json
c domain "domain set, prefix with a dot" $P domain set api my.api
c domain "domain ls after changes" $P domain ls
c domain "domain check acme.com" $P domain check acme.com
c domain "domain check acme.com --json" $P domain check acme.com --json
c domain "domain check unknown" $P domain check nope.example.org
m domain "domain rm shop.example.org" $P domain rm shop.example.org
m domain "domain rm docs.example.org --json" $P domain rm docs.example.org --json
c domain "domain rm unknown" $P domain rm nope.example.org
c domain "domain rm unknown --json" $P domain rm nope.example.org --json
c domain "domain ls --project nosuch" $P domain ls --project nosuch

# ---------- deploy ----------
note deploy "## deployment (seed: Deployment 1 queued; no Server answers)"
c deploy "deployment ls" $P deployment ls
c deploy "deployment ls --json" $P deployment ls --json
c deploy "deployment show 1" $P deployment show 1
c deploy "deployment show 1 --json" $P deployment show 1 --json
c deploy "deployment show unknown number" $P deployment show 99
c deploy "deployment show unknown number --json" $P deployment show 99 --json
c deploy "deployment show garbage id" $P deployment show not-an-id
m deploy "deployment start 1 --detach" $P deployment start 1 --detach
CAP_TIMEOUT=15 m deploy "deployment start 1 (follows; no Server)" $P deployment start 1
m deploy "deployment cancel 1" $P deployment cancel 1
m deploy "deployment cancel 1 again --json" $P deployment cancel 1 --json
CAP_TIMEOUT=15 m deploy "deployment retry 1 --detach" $P deployment retry 1 --detach
c deploy "deployment retry, unknown" $P deployment retry 99
note deploy "## deploy"
c deploy "deploy --plan" $P deploy --plan
c deploy "deploy --plan --json" $P deploy --plan --json
m deploy "deploy --detach --message ..." $P deploy --detach --message 'Ship it'
m deploy "deploy --detach --json" $P deploy --detach --json
CAP_TIMEOUT=15 m deploy "deploy (follows; Cloud refuses the Cluster Domain)" $P deploy
c deploy "deploy unknown service" $P deploy nosuch --plan
c deploy "deploy --expect-version malformed" $P deploy --expect-version 1 --detach
c deploy "deployment ls after deploys" $P deployment ls
note deploy "## logs / ps / link / up"
c deploy "ps" $P ps
c deploy "ps --json" $P ps --json
CAP_TIMEOUT=10 c deploy "logs" $P logs
CAP_TIMEOUT=10 c deploy "logs web --json" $P logs web --json
CAP_TIMEOUT=10 c deploy "logs --deployment 1 --build" $P logs --deployment 1 --build
need m deploy "link (from a fresh directory)" $P link
c deploy "status after link" $P status
cd "$WORK/up-app" || exit 1
CAP_TIMEOUT=20 m deploy "up in an empty directory" $P up
printf 'FROM nginx:alpine\n' >Dockerfile
CAP_TIMEOUT=20 m deploy "up in a directory with a Dockerfile --detach" $P up --detach
CAP_TIMEOUT=20 m deploy "up --json --detach" $P up --json --detach
cd "$WORK/app" || exit 1
note deploy "## cloud reset (no founding in progress)"
c deploy "cloud reset (no -y)" $P cloud reset
m deploy "cloud reset -y" $P cloud reset -y
m deploy "cloud reset -y --json" $P cloud reset -y --json

# ---------- logout (before the restart wipes the session) ----------
note auth "## logout"
m auth "logout" $P logout
c auth "logout again" $P logout
export HOME=$WORK/home-ada2; mkdir -p "$HOME"; signin
m auth "logout --json" $P logout --json

# Captures show the per-run work dir as /tmp/pz-cloud-capture.
FROM=$WORK perl -pi -e 's/\Q$ENV{FROM}\E/\/tmp\/pz-cloud-capture/g' "$CAPTURES"/*.md "$CAPTURES"/*.ansi
echo cloud >"$CAPTURES/.ok"
echo "cloud captures finished in $CAPTURES; publish with: $DOCS/publish.sh $CAPTURES"
