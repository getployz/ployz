#!/usr/bin/env bash
# Capture real `ployz` CLI output for commands that go through Ployz Cloud to a
# real Server.
#
# Prerequisites (manual): a paired setup, started from core/ with
#   run=$(scripts/verify-cluster up --machines 1 --daemon stable --cloud)
# then seeded with Organization `ada`, Project `shop`, Environment `production`
# running web (nginx, generated domain web.ada.ployz.test on port 80, custom
# domain acme.com), api, postgres (Volume pg-data) and worker, plus a staged,
# undeployed Branch `fix-api`. This script never starts, restarts or tears the
# setup down.
#
#   MANIFEST=/abs/core/.verify/<run>/cluster.json [SECTIONS="..."] run.sh [PARENT]
#   publish.sh DIR
# SECTIONS: deploy runtime domain env errors up (default: all, in that order).
# Each section writes its own OUT.md and OUT.ansi into a fresh mktemp -d under
# PARENT (default /tmp); `up` appends to deploy.md, so it runs only after `deploy`.
# publish.sh scrubs them and copies them over the tracked ones. Every section
# leaves web deployed and serving.
#
# The CLI only runs through `scripts/verify-cluster cli MANIFEST -- ARGS`, whose
# working directory is the run's workspace/; `up` deploys that directory, so
# the `up` section writes index.html there, then removes the Project it made
# and the link. If the run stops early, the EXIT trap puts links.json and
# index.html back as they were.
set -euo pipefail
shopt -s inherit_errexit
# Ctrl-C stops the run before .ok, even when the command it interrupted
# (script, ployz) swallowed the signal and exited normally.
trap 'exit 130' INT
trap 'exit 143' TERM
DOCS=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
CORE=$(cd -- "$DOCS/../.." && pwd) || exit 1
readonly DOCS CORE
: "${MANIFEST:=$(ls -d "$CORE"/.verify/*/cluster.json 2>/dev/null | tail -n1)}"
if [ -z "$MANIFEST" ] || ! jq -e '.cloud and .state == "ready"' "$MANIFEST" >/dev/null 2>&1; then
  echo "MANIFEST=${MANIFEST:-<none>} is not a ready paired run; start one with scripts/verify-cluster up --cloud, or set MANIFEST" >&2
  exit 1
fi
SECTIONS=${SECTIONS:-deploy runtime domain env errors up}
case " $SECTIONS " in
  *" up "*) [[ " $SECTIONS " == *" deploy "*"up "* ]] || { echo "SECTIONS: up appends to deploy.md, so deploy must come before it" >&2; exit 1; } ;;
esac
RUN_DIR=$(dirname "$MANIFEST")
WORKSPACE=$RUN_DIR/workspace
CAPTURES=$("$DOCS/capture-dir.sh" "${1:-}") || exit 1
# `verify-cluster cli` captures the CLI's output (as evidence) and replays it,
# so the CLI never sees a terminal there. The wrapper therefore takes that path
# when stdout is not a terminal, and otherwise runs the same binary with the
# same environment and working directory that `cli` sets, so TTY captures
# (cap.sh's `script` runs) show the real terminal rendering.
# PLOYZ_CLOUD_TIMEOUT=N stops a streaming command after N seconds either way.
WRAPPER=$(mktemp /tmp/ployz-cloud.XXXXXX) || exit 1
readonly SECTIONS RUN_DIR WORKSPACE CAPTURES WRAPPER
# up_section records here what it changed, so a run that stops early puts it back.
restore_links='' restore_index=''
# save FILE   copies FILE aside and prints the copy's path, or absent
save() {
  if [ ! -e "$1" ]; then echo absent; return; fi
  local copy
  copy=$(mktemp "$1.XXXXXX") || exit 1
  cp -p -- "$1" "$copy" || { rm -f -- "$copy"; exit 1; }
  echo "$copy"
}
# restore FILE SAVED   puts FILE back from SAVED, the output of save
restore() {
  if [ "$2" = absent ]; then rm -f -- "$1"; else mv -f -- "$2" "$1"; fi || echo "could not restore $1 from $2" >&2
}
cleanup() {
  rm -f "$WRAPPER"
  [ -z "$restore_links" ] || restore "$RUN_DIR/private/links.json" "$restore_links"
  [ -z "$restore_index" ] || restore "$WORKSPACE/index.html" "$restore_index"
}
trap cleanup EXIT
echo "raw captures in $CAPTURES"
{
  echo '#!/usr/bin/env bash'
  echo 'set -euo pipefail'
  printf 'vc=%q manifest=%q run=%q url=%q\n' "$CORE/scripts/verify-cluster" "$MANIFEST" "$RUN_DIR" "$(jq -r .cloud.url "$MANIFEST")"
  cat <<'WRAP'
if [ ! -t 1 ]; then
  exec "$vc" cli ${PLOYZ_CLOUD_TIMEOUT:+--timeout "$PLOYZ_CLOUD_TIMEOUT"} "$manifest" -- "$@"
fi
t=${PLOYZ_CLOUD_TIMEOUT:-}
for v in $(env | sed -n 's/^\(PLOYZ_[A-Z_]*\)=.*/\1/p'); do unset "$v"; done
token=$(cat "$run/private/cloud-token")
export PLOYZ_CONFIG="$run/private/config.yaml" PATH="$run/private/bin:$PATH"
export PLOYZ_TOKEN="$token" PLOYZ_CLOUD_URL="$url"
cd "$run/workspace" || exit 1
exec ${t:+timeout "$t"} "$run/artifacts/ployz" "$@"
WRAP
} >"$WRAPPER"
chmod +x "$WRAPPER"
cap=$DOCS/cap.sh
export PLOYZ_BIN=$WRAPPER
P=$PLOYZ_BIN
unset NO_COLOR
# A run that does not answer would turn every capture into a connection error.
"$P" --json deployment ls --limit 1 >/dev/null 2>&1 || { echo "the paired run in $MANIFEST does not answer" >&2; exit 1; }

c() { "$cap" "$@"; }              # pipe + TTY (read-only commands)
m() { CAP_TTY=0 "$cap" "$@"; }    # pipe only (state-changing commands)
t() { CAP_TTY=only "$cap" "$@"; } # TTY only (live rendering matters)
q() { "$P" "$@" >/dev/null 2>&1 || { echo "setup step failed: ployz $*" >&2; exit 1; }; } # setup step, not captured
try() { "$P" "$@" >/dev/null 2>&1 || echo "best-effort step failed: ployz $*" >&2; } # like q, but may fail
h() { echo "## $2" >>"$CAPTURES/$1"; echo >>"$CAPTURES/$1"; }
start() { : >"$CAPTURES/$1.ansi"; echo "# $2" >"$CAPTURES/$1.md"; echo >>"$CAPTURES/$1.md"; }
latest() { "$P" --json deployment ls --limit 1 | jq -r '.deployments[0].id'; }
latest_number() { "$P" --json deployment ls --limit 1 | jq -r '.deployments[0].number'; }
settle() { # wait until no Deployment of production is queued or running
  for _ in $(seq 60); do
    "$P" --json deployment ls --limit 5 | jq -e '[.deployments[] | select(.status == "queued" or .status == "running")] | length == 0' >/dev/null && return
    sleep 2
  done
  echo "settle: production still has a queued or running Deployment after 120s" >&2
  exit 1
}

deploy_section() {
  local f=deploy.md D=$CAPTURES/deploy.md
  start deploy "Deploy and Deployments through Cloud (real Server)"
  settle

  h $f "Nothing staged"
  c $D "deploy --plan, nothing staged" -- $P deploy --plan
  c $D "deploy --plan --json, nothing staged" -- $P --json deploy --plan
  m $D "deploy, nothing staged" -- $P deploy
  m $D "deploy --json, nothing staged" -- $P --json deploy

  h $f "Staged change"
  m $D "stage a change" -- $P set web.env.GREETING=hello
  c $D "diff" -- $P diff
  c $D "deploy --plan with a staged change" -- $P deploy --plan
  c $D "deploy --plan --json with a staged change" -- $P --json deploy --plan
  t $D "deploy a staged change (live follower)" -- $P deploy
  q set web.env.GREETING=hi
  m $D "deploy a staged change (pipe)" -- $P deploy --message "Say hi"
  q set web.env.GREETING=hey
  m $D "deploy --json a staged change" -- $P --json deploy

  h $f "Detach"
  q set web.env.GREETING=detached
  m $D "deploy --detach" -- $P deploy --detach
  settle
  q set web.env.GREETING=detached-json
  m $D "deploy --detach --json" -- $P --json deploy --detach
  settle

  h $f "Deployment ls and show"
  c $D "deployment ls" -- $P deployment ls --limit 5
  c $D "deployment ls --json" -- $P --json deployment ls --limit 2
  local id n; id=$(latest); n=$(latest_number)
  c $D "deployment show by id" -- $P deployment show "$id"
  c $D "deployment show by number" -- $P deployment show "$n"
  c $D "deployment show --json" -- $P --json deployment show "$id"

  h $f "A failing Deployment (bad image)"
  m $D "stage a bad image" -- $P set worker.image=ghcr.io/getployz/does-not-exist:nope
  m $D "deploy that fails" -- $P deploy
  t $D "deploy that fails (live follower)" -- $P deploy
  m $D "deploy --json that fails" -- $P --json deploy
  id=$(latest)
  c $D "deployment show of the failed Deployment" -- $P deployment show "$id"
  c $D "deployment show --json of the failed Deployment" -- $P --json deployment show "$id"
  m $D "deployment retry of the failed Deployment" -- $P deployment retry "$id"
  id=$(latest)
  m $D "deployment retry --json of the failed Deployment" -- $P --json deployment retry "$id"
  m $D "fix the image" -- $P set worker.image=traefik/whoami:v1.10.3
  m $D "deploy the fix" -- $P deploy

  h $f "Queued Deployments: cancel and start"
  # Three detached Deployments back to back: the first runs, the second is
  # superseded by the third, which waits queued behind the first.
  q set web.env.GREETING=queued-1; q deploy --detach
  q set web.env.GREETING=queued-2; q deploy --detach
  q deploy --detach
  c $D "deployment ls with queued Deployments" -- $P deployment ls --limit 4
  id=$(latest)
  m $D "deployment cancel a queued Deployment" -- $P deployment cancel "$id"
  settle
  q deploy --detach; q deploy --detach
  id=$(latest)
  m $D "deployment cancel --json a queued Deployment" -- $P --json deployment cancel "$id"
  settle
  q deploy --detach; q deploy --detach
  id=$(latest)
  m $D "deployment start a queued Deployment" -- $P deployment start "$id"
  settle
  q deploy --detach; q deploy --detach
  id=$(latest)
  m $D "deployment start --json a queued Deployment" -- $P --json deployment start "$id"
  settle
  # A running one: a fresh image pull keeps the Deployment running briefly.
  q set web.image=nginx:1.29-alpine; q deploy --detach
  sleep 1; id=$(latest)
  m $D "deployment cancel a running Deployment" -- $P deployment cancel "$id"
  settle
  c $D "deployment show of a cancelled Deployment" -- $P deployment show "$id"
  m $D "deployment retry of a cancelled Deployment" -- $P deployment retry "$id"
  settle
  q set web.image=nginx:1.27-alpine; q unset web.env.GREETING; q deploy
}

up_section() {
  local f=deploy.md D=$CAPTURES/deploy.md links=$RUN_DIR/private/links.json index=$WORKSPACE/index.html
  restore_links=$(save "$links") || exit 1
  restore_index=$(save "$index") || exit 1
  h $f "up from a directory with index.html"
  c $D "up --help" -- $P up --help
  printf '<h1>hello from ployz up</h1>\n' >"$WORKSPACE/index.html"
  m $D "up (pipe)" -- $P up
  printf '<h1>hello again</h1>\n' >"$WORKSPACE/index.html"
  t $D "up after a change (live follower)" -- $P up
  m $D "up --json" -- $P --json up
  c $D "status after up (two Projects)" -- $P status
  c $D "service ls after up (two Projects, no --project, linked directory)" -- $P service ls
  c $D "deployment ls after up" -- $P deployment ls
  m $D "project rm the up Project" -- $P project rm workspace --confirm workspace
  c $D "status with the directory still linked to the removed Project" -- $P status
  c $D "status --json with the directory still linked to the removed Project" -- $P --json status
  # No command unlinks a directory; drop the run's link so later commands act
  # on shop again.
  local next
  next=$(mktemp "$links.XXXXXX") || exit 1
  jq --arg k "$WORKSPACE" 'del(.[$k])' "$links" >"$next" || { rm -f -- "$next"; exit 1; }
  mv -f -- "$next" "$links"
  [ "$restore_links" = absent ] || rm -f -- "$restore_links"
  restore_links=''
  restore "$index" "$restore_index"
  restore_index=''
}

runtime_section() {
  local f=runtime.md R=$CAPTURES/runtime.md
  start runtime "Runtime commands through Cloud (real Server)"
  settle

  h $f "status"
  c $R "status" -- $P status
  c $R "status --json" -- $P --json status

  h $f "ps"
  c $R "ps" -- $P ps
  c $R "ps --json" -- $P --json ps

  h $f "logs"
  c $R "logs one Service" -- $P logs web --tail 5
  c $R "logs every Service" -- $P logs --tail 3
  c $R "logs --json one Service" -- $P --json logs web --tail 3
  c $R "logs --json every Service" -- $P --json logs --tail 2
  c $R "logs --utc --since" -- $P logs api --utc --since 2h --tail 3
  # Requests during each follow window give it new lines to stream.
  poke() { (sleep 4; for _ in 1 2; do try exec web -- wget -qO- http://localhost/; done) & }
  poke; PLOYZ_CLOUD_TIMEOUT=10 m $R "logs --follow (stopped after 10s; exit 124 is that timeout)" -- $P logs web --follow --tail 2; wait
  poke; PLOYZ_CLOUD_TIMEOUT=10 t $R "logs --follow (stopped after 10s)" -- $P logs web --follow --tail 2; wait
  poke; PLOYZ_CLOUD_TIMEOUT=10 m $R "logs --follow --json (stopped after 10s)" -- $P --json logs web --follow --tail 2; wait
  local id; id=$(latest)
  c $R "logs --deployment" -- $P logs --deployment "$id" --tail 2

  h $f "exec"
  c $R "exec a command" -- $P exec web -- nginx -v
  c $R "exec a command with output" -- $P exec web -- cat /etc/os-release
  c $R "exec --json" -- $P --json exec web -- nginx -v
  c $R "exec a failing command" -- $P exec web -- sh -c 'echo to-stderr >&2; exit 7'

  h $f "service inspect"
  c $R "service inspect" -- $P service inspect web
  c $R "service inspect --json" -- $P --json service inspect web
  c $R "service ls" -- $P service ls

  h $f "service restart, stop, start"
  m $R "service restart" -- $P service restart web
  m $R "service restart --json" -- $P --json service restart web
  m $R "service stop" -- $P service stop worker
  c $R "ps with a stopped Service" -- $P ps
  m $R "service start" -- $P service start worker
  m $R "service stop --json" -- $P --json service stop worker
  m $R "service start --json" -- $P --json service start worker
  m $R "service restart two Services" -- $P service restart api worker

  h $f "server"
  c $R "server ls" -- $P server ls
  c $R "server ls --json" -- $P --json server ls
  c $R "server inspect" -- $P server inspect machine-1
  c $R "server inspect --json" -- $P --json server inspect machine-1
  c $R "server logs" -- $P server logs --tail 5
  c $R "server logs --json" -- $P --json server logs --tail 2
}

domain_section() {
  local f=domain.md M=$CAPTURES/domain.md
  start domain "Domains through Cloud (real Server, fake Hosted DNS)"
  settle

  h $f "ls and check"
  c $M "domain ls" -- $P domain ls
  c $M "domain ls --json" -- $P --json domain ls
  c $M "domain check the generated domain" -- $P domain check web.ada.ployz.test
  c $M "domain check --json the generated domain" -- $P --json domain check web.ada.ployz.test
  c $M "domain check a custom domain" -- $P domain check acme.com
  c $M "domain check --json a custom domain" -- $P --json domain check acme.com

  h $f "Change the generated domain"
  m $M "domain set" -- $P domain set web shopweb
  c $M "domain ls with a staged change" -- $P domain ls
  c $M "diff" -- $P diff
  m $M "deploy" -- $P deploy
  c $M "domain ls after deploy" -- $P domain ls
  m $M "domain set --json back" -- $P --json domain set web web --port 80
  m $M "deploy" -- $P deploy

  h $f "Add and remove a generated domain"
  m $M "domain add a generated domain" -- $P domain add api
  m $M "deploy" -- $P deploy
  c $M "domain ls with two generated domains" -- $P domain ls
  local host; host=$("$P" --json domain ls | jq -r '[.. | objects | select(has("host")) | .host | select(startswith("api"))][0] // empty')
  m $M "domain rm" -- $P domain rm "${host:-api.ada.ployz.test}"
  m $M "deploy" -- $P deploy
  m $M "domain add --json a generated domain" -- $P --json domain add api
  m $M "domain rm --json" -- $P --json domain rm "${host:-api.ada.ployz.test}"
  m $M "discard" -- $P discard

  h $f "Remove a custom domain, then discard"
  m $M "domain rm a custom domain" -- $P domain rm acme.com
  c $M "domain ls with a staged removal" -- $P domain ls
  m $M "discard" -- $P discard
  m $M "domain add a custom domain that exists" -- $P domain add web acme.com
}

env_section() {
  local f=env.md E=$CAPTURES/env.md
  start env "Environments and Branches through Cloud (deployed Parent)"
  settle
  # Leftovers from an earlier run; usually there are none to remove.
  try env rm hotfix --confirm shop/hotfix; try env rm preview --confirm shop/preview

  # A Branch uses a node live only when a node it copies references it, so
  # give production's worker a reference to api first (undone at the end).
  q set 'worker.env.API_HOST=${{ api.PLOYZ_PRIVATE_DOMAIN }}'; q deploy

  h $f "ls"
  c $E "env ls" -- $P env ls
  c $E "env ls --json" -- $P --json env ls

  h $f "branch"
  m $E "env branch --live of a node nothing copied uses (refused)" -- $P env branch hotfix --live postgres --copy api
  m $E "env branch --json --live of a node nothing copied uses (refused)" -- $P --json env branch hotfix --live postgres --copy api
  m $E "env branch --copy (api, which worker references, stays live)" -- $P env branch hotfix --copy worker
  m $E "env branch --json --copy --live (a Volume by its bare name, refused)" -- $P --json env branch preview --copy postgres --live pg-data
  m $E "env branch --json --live of a Volume the Branch copies (refused)" -- $P --json env branch preview --copy postgres --live volumes.pg-data
  m $E "env branch --json --copy" -- $P --json env branch preview --copy web
  c $E "env ls with Branches" -- $P env ls
  c $E "service ls in a Branch" -- $P service ls --env hotfix
  c $E "diff in a Branch" -- $P diff --env hotfix
  m $E "deploy the Branch" -- $P deploy --env hotfix
  c $E "ps in a Branch" -- $P ps --env hotfix

  h $f "copy"
  m $E "env copy a Live Node" -- $P env copy api --env hotfix
  c $E "diff after env copy" -- $P diff --env hotfix
  m $E "env copy --json a Volume (refused: takes a Service)" -- $P --json env copy volumes.pg-data --env hotfix
  m $E "env copy --json a Live Node, already copied" -- $P --json env copy api --env hotfix
  m $E "env copy a node that is not live" -- $P env copy worker --env hotfix
  m $E "deploy the Branch after copy" -- $P deploy --env hotfix

  h $f "sync"
  q set api.env.LOG_LEVEL=info --env hotfix
  c $E "env sync --plan to the Parent" -- $P env sync --env hotfix --to --plan
  c $E "env sync --plan --json to the Parent" -- $P --json env sync --env hotfix --to --plan
  m $E "env sync to the Parent" -- $P env sync --env hotfix --to
  c $E "diff in production after sync" -- $P diff
  m $E "discard in production" -- $P discard
  q set api.env.LOG_LEVEL=error --env hotfix
  m $E "env sync --json to the Parent" -- $P --json env sync --env hotfix --to
  q discard
  m $E "env sync --from the Parent" -- $P env sync --env fix-api --from production --plan
  c $E "env sync --plan of the seeded fix-api Branch" -- $P env sync --env fix-api --to --plan

  h $f "rm"
  m $E "env rm without --confirm" -- $P env rm hotfix
  m $E "env rm" -- $P env rm hotfix --confirm shop/hotfix
  m $E "env rm --json" -- $P --json env rm preview --confirm shop/preview
  c $E "env ls after" -- $P env ls

  q unset worker.env.API_HOST; q deploy
}

errors_section() {
  local f=errors.md X=$CAPTURES/errors.md
  start errors "Errors through Cloud (real Server)"
  settle
  local j
  for j in "" --json; do
    h $f "Errors ${j:-human}"
    c $X "logs of an unknown Service $j" -- $P $j logs nosuch
    c $X "exec in an unknown Service $j" -- $P $j exec nosuch -- true
    c $X "exec a missing binary $j" -- $P $j exec web -- no-such-binary
    q service stop worker
    c $X "exec in a stopped Service $j" -- $P $j exec worker -- true
    c $X "logs of a stopped Service $j" -- $P $j logs worker --tail 2
    q service start worker
    c $X "service restart of an unknown Service $j" -- $P $j service restart nosuch
    c $X "service inspect of an unknown Service $j" -- $P $j service inspect nosuch
    c $X "deploy an unknown Service $j" -- $P $j deploy nosuch
    c $X "deploy with a stale --expect-version $j" -- $P $j deploy --expect-version 1:1:0.0
    c $X "deployment show of an unknown id $j" -- $P $j deployment show 00000000-0000-0000-0000-000000000000
    c $X "deployment show of an unknown number $j" -- $P $j deployment show 9999
    c $X "deployment retry of an applied Deployment $j" -- $P $j deployment retry "$(latest)"
    c $X "deployment cancel of an applied Deployment $j" -- $P $j deployment cancel "$(latest)"
    c $X "deployment start of an applied Deployment $j" -- $P $j deployment start "$(latest)"
    c $X "domain check of an unknown domain $j" -- $P $j domain check nosuch.example.com
    c $X "domain set on an unknown Service $j" -- $P $j domain set nosuch foo
    c $X "domain rm of an unknown domain $j" -- $P $j domain rm nosuch.example.com
    c $X "server inspect of an unknown Server $j" -- $P $j server inspect nosuch
    c $X "env copy of an unknown node $j" -- $P $j env copy nosuch --env fix-api
    c $X "env copy in a non-Branch $j" -- $P $j env copy web
    c $X "env sync --to from a non-Branch $j" -- $P $j env sync --to --plan
    c $X "ps in an unknown Environment $j" -- $P $j ps --env nosuch
    c $X "logs --since garbage $j" -- $P $j logs web --since yesterday
  done
}

for s in $SECTIONS; do "${s}_section"; done
settle
"$P" ps
# The run directory differs per checkout and run, so captures show it as <run-dir>.
FROM=$RUN_DIR perl -pi -e 's/\Q$ENV{FROM}\E/<run-dir>/g' "$CAPTURES"/*.md "$CAPTURES"/*.ansi
echo cloud-cluster >"$CAPTURES/.ok"
echo "cloud-cluster captures finished in $CAPTURES; publish with: $DOCS/publish.sh $CAPTURES"
