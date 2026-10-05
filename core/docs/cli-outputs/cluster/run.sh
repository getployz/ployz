#!/usr/bin/env bash
# Capture real `ployz` CLI output against a throwaway 4-Machine testkit Cluster
# (alpha, beta, gamma, plus a lone solo).
#
# Prerequisites (manual):
#   - Docker that can run --privileged containers.
#   - Debug binaries of this checkout: (cd core && cargo build -p ployz -p ployzd)
#   - Base testkit image: docker pull ghcr.io/getployz/ployz-testkit:main
#     This script layers this checkout's debug ployzd/ployz on top of it as
#     ployz-cli-capture:ID, so the daemon matches the CLI.
#
# Nothing touches ~/.config/ployz or real Servers: HOME is a fresh
# /tmp/ployz-cap-home-ID.XXXXXX, the Config Store is the hidden SQLite one
# (PLOYZ_STORE), and every container and network carries the testkit labels
# (dev.ployz.testkit=true, dev.ployz.testkit.cluster=cli-capture-ID). ID is
# random per run. Teardown runs on exit unless KEEP=1; to clean a kept run by hand:
#   docker rm -f $(docker ps -aq -f label=dev.ployz.testkit.cluster=cli-capture-ID)
#   docker network rm ployz-testkit-cli-capture-ID; docker image rm ployz-cli-capture:ID
#
# The Store-backed `ployz deploy` follower prints plain status lines even on a
# TTY; only the direct apply path (Ingress in server add/set) redraws.
#
#   cluster/run.sh [PARENT]   raw captures into a fresh mktemp -d under PARENT (default /tmp)
#   publish.sh DIR         scrubs them and copies them over the tracked ones
set -euo pipefail
shopt -s inherit_errexit
# Ctrl-C stops the run before .ok, even when the command it interrupted
# (script, ployz) swallowed the signal and exited normally.
trap 'exit 130' INT
trap 'exit 143' TERM
DOCS=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
CORE=$(cd -- "$DOCS/../.." && pwd) || exit 1
CAPTURES=$("$DOCS/capture-dir.sh" "${1:-}") || exit 1
# Every name this run creates carries a per-run id, so two checkouts never
# tear down each other's Cluster, image or HOME.
ID=$(od -An -N4 -tx4 /dev/urandom | tr -d ' ')
NAME=cli-capture-$ID
NET=ployz-testkit-$NAME
IMAGE=ployz-cli-capture:$ID
HOME=$(mktemp -d "/tmp/ployz-cap-home-$ID.XXXXXX") || exit 1
readonly DOCS CORE CAPTURES ID NAME NET IMAGE HOME
export HOME
build_ctx=''
teardown() {
  [ -z "$build_ctx" ] || rm -rf "$build_ctx"
  [ "${KEEP:-0}" != 1 ] || return 0
  docker ps -aq -f "label=dev.ployz.testkit.cluster=$NAME" | xargs -r docker rm -f -v >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  docker image rm "$IMAGE" >/dev/null 2>&1 || true
  rm -rf "$HOME"
}
trap teardown EXIT
echo "run id $ID (cluster label $NAME); raw captures in $CAPTURES"
cap=$DOCS/cap.sh
export PLOYZ_BIN=${PLOYZ_BIN:-$CORE/target/debug/ployz}
P=$PLOYZ_BIN
COUNT=4
export PLOYZ_STORE="sqlite:$HOME/store.db"
unset NO_COLOR
unset PLOYZ_PROJECT PLOYZ_ENV PLOYZ_CONTEXT PLOYZ_CONNECT PLOYZ_CONFIG PLOYZ_TOKEN PLOYZ_CLOUD_URL XDG_CONFIG_HOME

c() { "$cap" "$@"; }              # pipe + TTY (read-only commands)
m() { CAP_TTY=0 "$cap" "$@"; }    # pipe only (state-changing commands)
t() { CAP_TTY=only "$cap" "$@"; } # TTY only (live rendering matters)
need() { CAP_MUST=1 "$@"; }       # later captures depend on this one succeeding
head_json() { # capture the first N lines of a streaming --json command
  local file=$1 title=$2 n=$3 cmd; shift 4
  printf -v cmd '%q ' "$@"
  { echo "### $title"; echo; echo '```console'; echo "\$ ${cmd//$PLOYZ_BIN/ployz}| head -$n"; echo '```'; echo
    echo 'stdout (first lines):'; echo '```'; timeout 15 "$@" 2>&1 </dev/null | head -"$n" || true; echo '```'; echo; } >>"$file"
}
ip_of() { docker inspect -f "{{(index .NetworkSettings.Networks \"$NET\").IPAddress}}" "$NET-$1"; }

# --- Cluster ----------------------------------------------------------------
mkdir -p "$HOME/.ssh" "$HOME/bin"; ssh-keygen -q -t ed25519 -N '' -f "$HOME/.ssh/id_ed25519"
# ssh reads ~ from passwd, not $HOME: pin known_hosts to the throwaway HOME.
printf 'Host *\n  UserKnownHostsFile %s/.ssh/known_hosts\n  IdentityFile %s/.ssh/id_ed25519\n' "$HOME" "$HOME" >"$HOME/.ssh/config"
printf '#!/bin/sh\nexec /usr/bin/ssh -F %s/.ssh/config "$@"\n' "$HOME" >"$HOME/bin/ssh"; chmod +x "$HOME/bin/ssh"
export PATH="$HOME/bin:$PATH"
build_ctx=$(mktemp -d) || exit 1
cp "$CORE/target/debug/ployzd" "$CORE/target/debug/ployz" "$build_ctx/"
printf 'FROM ghcr.io/getployz/ployz-testkit:main\nCOPY ployzd ployz /usr/local/bin/\n' >"$build_ctx/Dockerfile"
docker build -q -t "$IMAGE" "$build_ctx" >/dev/null
rm -rf "$build_ctx"
docker network create --label dev.ployz.testkit=true --label "dev.ployz.testkit.cluster=$NAME" "$NET" >/dev/null
for i in $(seq 0 $((COUNT - 1))); do
  docker run -d --privileged --name "$NET-$i" --network "$NET" \
    --label dev.ployz.testkit=true --label "dev.ployz.testkit.cluster=$NAME" \
    --env PLOYZ_ACME_DIRECTORY= "$IMAGE" >/dev/null
done
ready() { # ready CONTAINER SECS: wait for the daemon socket, or stop the run
  for _ in $(seq "$2"); do docker exec "$1" test -S /run/ployz/ployz.sock 2>/dev/null && return; sleep 1; done
  echo "$1: daemon socket missing after $2s" >&2
  exit 1
}
for i in $(seq 0 $((COUNT - 1))); do
  ready "$NET-$i" 120
  docker exec -i "$NET-$i" sh -c 'cat >> /root/.ssh/authorized_keys' <"$HOME/.ssh/id_ed25519.pub"
done
IP0=$(ip_of 0); IP1=$(ip_of 1); IP2=$(ip_of 2); IP3=$(ip_of 3)

# --- server add (setup/provisioning output) ---------------------------------
S=$CAPTURES/server.md
need m "$S" "server add: found a Cluster (pipe)" -- $P server add --standalone --no-install "root@$IP0" --name alpha
need t "$S" "server add: join a second Server" -- $P server add --standalone --no-install "root@$IP1" --name beta
need m "$S" "server add: join a third Server (--json)" -- $P server add --standalone --no-install "root@$IP2" --name gamma --json
# Machine 3 founds a separate context under a TTY: the only place the rich
# apply renderer (redraws + color) runs, for the Ingress Proxy.
need t "$S" "server add: found a second, separate Cluster (rich ingress progress)" -- $P server add --standalone --no-install "root@$IP3" --name solo --ployz-config "$HOME/solo.yaml"
m "$S" "server add: unreachable destination" -- $P server add --standalone --no-install root@10.255.255.1 --ssh-timeout 3
m "$S" "server add: without --standalone (needs Cloud login)" -- $P server add "root@$IP2"

# --- server read commands ---------------------------------------------------
c "$S" "server ls" -- $P server ls
c "$S" "server ls --json" -- $P server ls --json
c "$S" "server inspect" -- $P server inspect alpha
c "$S" "server inspect --json" -- $P server inspect alpha --json
c "$S" "server logs -n 5" -- $P server logs -n 5
head_json "$S" "server logs --json" 5 -- $P server logs -n 5 --json
c "$S" "server clean (list orphan Namespaces)" -- $P server clean
c "$S" "server clean --json" -- $P server clean --json
c "$S" "server build-cache-clear" -- $P server build-cache-clear
c "$S" "server build-cache-clear --json" -- $P server build-cache-clear --json
m "$S" "server set (labels)" -- $P server set gamma --label-add zone=b
m "$S" "server set --json" -- $P server set gamma --label-rm zone --json
m "$S" "server set (no change flags)" -- $P server set gamma
m "$S" "server upgrade (same version)" -- $P server upgrade 0.2.3 gamma
m "$S" "server forget (standalone)" -- $P server forget

# --- Project + Services through the hidden Store, then deploy ----------------
D=$CAPTURES/deploy.md
need m "$D" "project new" -- $P project new shop
need m "$D" "service add web" -- $P service add web --image alpine:3.23.3
need m "$D" "set web.startCommand" -- $P set "web.startCommand=sh -c 'echo started; while true; do echo tick; sleep 2; done'"
need m "$D" "set web.replicas" -- $P set web.replicas=2
need m "$D" "service add api" -- $P service add api --image alpine:3.23.3
need m "$D" "set api.startCommand" -- $P set "api.startCommand=sh -c 'echo api up; sleep 100000'"
c "$D" "deploy --plan" -- $P deploy --plan
c "$D" "deploy --plan --json" -- $P deploy --plan --json
need m "$D" "deploy (pipe, first deploy)" -- $P deploy --message "first deploy"
need m "$D" "set web.env (staged change)" -- $P set web.env.GREETING=hello
need t "$D" "deploy (live progress)" -- $P deploy --message "greeting"
need m "$D" "set api.env (staged change)" -- $P set api.env.MODE=fast
need m "$D" "deploy --json --events (result + NDJSON progress)" -- $P deploy --json --events "$HOME/events.ndjson"
{ echo '### deploy --events NDJSON (first 15 lines)'; echo; echo '```'; head -15 "$HOME/events.ndjson"; echo '```'; echo; } >>"$D"
c "$D" "deploy with nothing staged" -- $P deploy
m "$D" "deploy --detach" -- $P deploy --detach
c "$D" "deployment ls" -- $P deployment ls
c "$D" "deployment ls --json" -- $P deployment ls --json
DEP=$($P deployment ls --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["deployments"][-1]["id"])')
c "$D" "deployment show" -- $P deployment show "$DEP"
c "$D" "deployment show --json" -- $P deployment show "$DEP" --json
c "$D" "deployment show unknown" -- $P deployment show 01ZZZZZZZZZZZZZZZZZZZZZZZZ
m "$D" "set bad image" -- $P set web.image=alpine:does-not-exist
t "$D" "deploy that fails (bad image, live progress)" -- $P deploy
m "$D" "deploy that fails (pipe)" -- $P deploy
need m "$D" "restore image" -- $P set web.image=alpine:3.23.3
need m "$D" "deploy restore" -- $P deploy

# --- service lifecycle -------------------------------------------------------
V=$CAPTURES/service.md
c "$V" "service ls" -- $P service ls
c "$V" "service inspect" -- $P service inspect web
c "$V" "service inspect --json" -- $P service inspect web --json
m "$V" "service stop" -- $P service stop api
m "$V" "service start" -- $P service start api
m "$V" "service restart" -- $P service restart web
m "$V" "service restart --json" -- $P service restart api --json
m "$V" "service stop unknown" -- $P service stop nope
rc=0; timeout 4 $P service port-forward web 0:80 >"$HOME/pf.out" 2>"$HOME/pf.err" || rc=$?; echo "# exit $rc" >>"$HOME/pf.err"
{ echo '### service port-forward (killed after 4s)'; echo; echo '```console'; echo '$ ployz service port-forward web 0:80'; echo '```'; echo
  echo 'stdout:'; echo '```'; cat "$HOME/pf.out"; echo '```'; echo; echo 'stderr:'; echo '```'; cat "$HOME/pf.err"; echo '```'; echo; } >>"$V"
rc=0; timeout 4 $P service port-forward web 0:80 --json >"$HOME/pf.out" 2>"$HOME/pf.err" || rc=$?; echo "# exit $rc" >>"$HOME/pf.err"
{ echo '### service port-forward --json (killed after 4s)'; echo; echo 'stdout:'; echo '```'; cat "$HOME/pf.out"; echo '```'; echo; echo 'stderr:'; echo '```'; cat "$HOME/pf.err"; echo '```'; echo; } >>"$V"

# --- ps / logs / exec -------------------------------------------------------
L=$CAPTURES/ps-logs-exec.md
c "$L" "ps" -- $P ps
c "$L" "ps --json" -- $P ps --json
c "$L" "logs web -n 3" -- $P logs web -n 3
c "$L" "logs (all Services) -n 2" -- $P logs -n 2
head_json "$L" "logs --json" 4 -- $P logs web -n 2 --json
c "$L" "logs --deployment" -- $P logs --deployment "$DEP" -n 2
c "$L" "logs unknown service" -- $P logs nope
c "$L" "exec" -- $P exec web -- echo hello from exec
c "$L" "exec --json" -- $P exec web --json -- echo hi
c "$L" "exec failing command" -- $P exec web -- sh -c 'echo oops >&2; exit 3'
c "$L" "exec unknown service" -- $P exec nope -- true

# --- volumes ----------------------------------------------------------------
O=$CAPTURES/volume.md
m "$O" "volume add --docker on a 2-replica Service (refused)" -- $P volume add data --docker --mount web:/data
m "$O" "volume add --docker" -- $P volume add data --docker --mount api:/data
m "$O" "volume add managed (--json)" -- $P volume add cache --size 1 --mount api:/cache --json
m "$O" "deploy with a Managed volume and no ZFS Server (fails)" -- $P deploy
c "$O" "volume ls" -- $P volume ls
c "$O" "volume ls --json" -- $P volume ls --json
c "$O" "volume inspect" -- $P volume inspect data
c "$O" "volume inspect --json" -- $P volume inspect data --json
m "$O" "volume set --shared-writes" -- $P volume set data --shared-writes
m "$O" "volume set --docker (managed -> docker)" -- $P volume set cache --docker
m "$O" "volume rename" -- $P volume rename data store
m "$O" "volume rm (never-deployed Managed volume)" -- $P volume rm cache
m "$O" "deploy with volumes" -- $P deploy
c "$O" "volume ls after deploy" -- $P volume ls
m "$O" "volume rm" -- $P volume rm store
m "$O" "deploy refused: volume loss" -- $P deploy
m "$O" "deploy refused: volume loss (--json)" -- $P deploy --json
m "$O" "volume inspect unknown" -- $P volume inspect nope

m "$O" "build without a Cloud Build Grant" -- $P build --grant x --deployment 1 --commit abc --fingerprint f

# --- partial fan-out and errors ---------------------------------------------
E=$CAPTURES/errors.md
c "$E" "server inspect unknown" -- $P server inspect nope
c "$E" "service inspect unknown" -- $P service inspect nope
c "$E" "server rm without --confirm" -- $P server rm gamma
c "$E" "server drain unknown" -- $P server drain nope
docker stop "$NET-2" >/dev/null
c "$E" "server ls (gamma stopped)" -- $P server ls
c "$E" "server ls --json (gamma stopped)" -- $P server ls --json
c "$E" "server inspect gamma (stopped)" -- $P server inspect gamma
c "$E" "ps (gamma stopped)" -- $P ps
c "$E" "ps --json (gamma stopped)" -- $P ps --json
c "$E" "logs (gamma stopped)" -- $P logs web -n 1
c "$E" "server logs (gamma stopped)" -- $P server logs -n 1
m "$E" "service restart (gamma stopped)" -- $P service restart web
docker start "$NET-2" >/dev/null
ready "$NET-2" 60
sleep 5

# --- drain and rm ------------------------------------------------------------
t "$S" "server drain (live)" -- $P server drain beta
m "$S" "server drain --json (rerun)" -- $P server drain beta --json
m "$S" "server set --accepts-services=true" -- $P server set beta --accepts-services=true
m "$S" "server rm --confirm" -- $P server rm gamma --confirm gamma
m "$S" "server rm --json (unknown now)" -- $P server rm gamma --confirm gamma --json
c "$S" "server ls after rm" -- $P server ls
# Captures show the per-run HOME as /tmp/ployz-cap-home, so reruns diff cleanly.
FROM=$HOME perl -pi -e 's/\Q$ENV{FROM}\E/\/tmp\/ployz-cap-home/g' "$CAPTURES"/*.md "$CAPTURES"/*.ansi
echo cluster >"$CAPTURES/.ok"
echo "cluster captures finished in $CAPTURES; publish with: $DOCS/publish.sh $CAPTURES"
