#!/usr/bin/env bash
# Fresh-host acceptance: Rocky 9 amd64 core + Ubuntu 24.04 arm64 edge.
# See docs/operations/real-host-acceptance.md before running.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)

CORE="${1:?usage: real-host-acceptance.sh <core-ip> <edge-ip>}"
EDGE="${2:?usage: real-host-acceptance.sh <core-ip> <edge-ip>}"
SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o GSSAPIAuthentication=no \
          -o ConnectTimeout=20 -o ServerAliveInterval=15)

ts() { date +%s; }
log() { echo "[$(date +%H:%M:%S)] $*"; }
remote() { ssh "${SSH_OPTS[@]}" "root@$1" "${@:2}"; }
core() { remote "$CORE" "$@"; }
managed_host() { grep -Eo '[A-Za-z0-9.-]+\.up\.ployz\.app' | tail -1 || true; }
valid_ipv4() {
  local part
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1
  IFS=. read -r -a parts <<<"$1"
  for part in "${parts[@]}"; do (( 10#$part <= 255 )) || return 1; done
}

valid_ipv4 "$CORE" || { log "core must be a literal IPv4 address"; exit 1; }
valid_ipv4 "$EDGE" || { log "edge must be a literal IPv4 address"; exit 1; }

stopped_host=
stopped_container=
probe_dir=
probe_pid=
git_fixture_dir=
cleanup() {
  if [ -n "$probe_pid" ]; then
    touch "$probe_dir/stop"
    wait "$probe_pid" 2>/dev/null || true
  fi
  if [ -n "$stopped_container" ]; then
    remote "$stopped_host" "docker start '${stopped_container}' >/dev/null" || true
  fi
  [ -z "$probe_dir" ] || rm -rf "$probe_dir"
  if [ -n "$git_fixture_dir" ]; then
    core 'systemctl stop ployz-real-host-build-git.service 2>/dev/null || true; firewall-cmd --quiet --remove-port=9443/tcp 2>/dev/null || true; rm -rf /tmp/ployz-authenticated-git-server.py /tmp/ployz-build-git /etc/pki/ca-trust/source/anchors/ployz-build-git.crt; update-ca-trust >/dev/null 2>&1 || true' || true
    remote "$EDGE" 'rm -f /usr/local/share/ca-certificates/ployz-build-git.crt; update-ca-certificates >/dev/null 2>&1 || true' || true
    rm -rf "$git_fixture_dir"
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

log "waiting for both hosts to accept SSH"
for host in "$CORE" "$EDGE"; do
  ready=0
  for _ in $(seq 1 50); do
    if remote "$host" true 2>/dev/null; then ready=1; break; fi
    sleep 6
  done
  [ "$ready" = 1 ] || { log "SSH unavailable on $host"; exit 1; }
done

core_arch=$(core 'uname -m')
edge_arch=$(remote "$EDGE" 'uname -m')
[ "$core_arch" = x86_64 ] || { log "core must be amd64 (x86_64), got $core_arch"; exit 1; }
case "$edge_arch" in aarch64|arm64) ;; *) log "edge must be arm64, got $edge_arch"; exit 1;; esac
core 'grep -qE "^(ID|ID_LIKE)=.*(rhel|fedora|rocky)" /etc/os-release' || {
  log "core must be Rocky/RHEL-family"; exit 1;
}
remote "$EDGE" 'grep -q "^ID=ubuntu" /etc/os-release' || {
  log "edge must be Ubuntu"; exit 1;
}

# Rocky images normally start firewalld. Ubuntu images normally ship UFW
# inactive, so allow SSH before enabling it. Keeper owns the Ployz port rules.
core 'systemctl is-active --quiet firewalld' || {
  log "core firewalld must already be active"; exit 1;
}
remote "$EDGE" 'ufw allow OpenSSH >/dev/null && ufw --force enable >/dev/null'

# The operator shells out to ssh for init/add.
for host in "$CORE" "$EDGE"; do
  remote "$host" 'mkdir -p ~/.ssh; printf "Host *\n  GSSAPIAuthentication no\n  StrictHostKeyChecking accept-new\n" > ~/.ssh/config; chmod 600 ~/.ssh/config'
done
core '[ -f ~/.ssh/id_ed25519 ] || ssh-keygen -t ed25519 -N "" -f ~/.ssh/id_ed25519 -q; grep -qF "$(cat ~/.ssh/id_ed25519.pub)" ~/.ssh/authorized_keys || cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys'
core "ssh-keyscan -T 8 ${CORE} 127.0.0.1 >> ~/.ssh/known_hosts 2>/dev/null"

log "installing the public alpha CLI on the core"
core "timeout 10m sh -c 'curl --connect-timeout 20 --max-time 120 -fsSL https://ployz.sh | sh -s -- --channel alpha >/dev/null'"
log "version: $(core 'grep -h PLOYZ_VERSION /etc/ployz/release.env')"

log "machine init (amd64 Rocky core)"
t0=$(ts)
core "timeout 15m ployz machine init root@${CORE} --name ployz-core --public-ip ${CORE}"
log "TIMING machine-init=$(( $(ts)-t0 ))s"

core_key=$(core 'cat ~/.ssh/id_ed25519.pub')
remote "$EDGE" "grep -qF '${core_key}' ~/.ssh/authorized_keys 2>/dev/null || echo '${core_key}' >> ~/.ssh/authorized_keys"
core "ssh-keyscan -T 8 ${EDGE} >> ~/.ssh/known_hosts 2>/dev/null"

log "machine add (arm64 Ubuntu edge)"
t0=$(ts)
core "timeout 15m ployz machine add root@${EDGE} --name ployz-edge"
log "TIMING machine-add=$(( $(ts)-t0 ))s"
core 'ployz machine list'

log "preparing authenticated exact-commit Git build fixture"
git_fixture_dir=$(mktemp -d)
scp "${SSH_OPTS[@]}" \
  "$REPO_ROOT/testing/ployz-e2e/tests/dind_cluster/fixtures/authenticated_git_server.py" \
  "root@${CORE}:/tmp/ployz-authenticated-git-server.py" >/dev/null
ssh "${SSH_OPTS[@]}" "root@${CORE}" 'bash -s' <<SETUP_GIT
set -euo pipefail
rm -rf /tmp/ployz-build-git
mkdir -p /tmp/ployz-build-git/work/dockerfile /tmp/ployz-build-git/work/railpack /tmp/ployz-build-git/work/slow
mv /tmp/ployz-authenticated-git-server.py /tmp/ployz-build-git/server.py
chmod 0700 /tmp/ployz-build-git/server.py
printf '%s\n' 'FROM alpine:3.20' 'COPY marker /marker' 'CMD ["sh", "-c", "while true; do sleep 600; done"]' > /tmp/ployz-build-git/work/dockerfile/Dockerfile
printf '%s\n' 'real-host exact Dockerfile commit' > /tmp/ployz-build-git/work/dockerfile/marker
printf '%s\n' '{"scripts":{"start":"node server.js"},"engines":{"node":"22"}}' > /tmp/ployz-build-git/work/railpack/package.json
printf '%s\n' 'require("http").createServer((_, res) => res.end("real-host railpack\\n")).listen(process.env.PORT || 3000);' > /tmp/ployz-build-git/work/railpack/server.js
printf '%s\n' 'FROM alpine:3.20' 'RUN echo blocking-build-start && sleep 600' 'CMD ["true"]' > /tmp/ployz-build-git/work/slow/Dockerfile
git -C /tmp/ployz-build-git/work init -q -b main
git -C /tmp/ployz-build-git/work config user.name 'Ployz acceptance'
git -C /tmp/ployz-build-git/work config user.email 'acceptance@example.invalid'
git -C /tmp/ployz-build-git/work add .
GIT_AUTHOR_DATE='2026-01-01T00:00:00Z' GIT_COMMITTER_DATE='2026-01-01T00:00:00Z' git -C /tmp/ployz-build-git/work commit -qm fixture
git clone -q --bare /tmp/ployz-build-git/work /tmp/ployz-build-git/repo.git
git -C /tmp/ployz-build-git/repo.git update-server-info
git -C /tmp/ployz-build-git/work rev-parse HEAD > /tmp/ployz-build-git/commit
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj '/CN=${CORE}' -addext 'subjectAltName=IP:${CORE}' -keyout /tmp/ployz-build-git/server.key -out /tmp/ployz-build-git/server.crt >/dev/null 2>&1
install -m 0644 /tmp/ployz-build-git/server.crt /etc/pki/ca-trust/source/anchors/ployz-build-git.crt
update-ca-trust
firewall-cmd --quiet --add-port=9443/tcp
printf '%s\n' 'PLOYZ_BUILD_GIT_SECRET=build-secret-370' > /tmp/ployz-build-git/secret.env
chmod 0600 /tmp/ployz-build-git/secret.env
systemd-run --unit=ployz-real-host-build-git --setenv=GIT_USERNAME=builder --setenv=GIT_PASSWORD=build-secret-370 /usr/bin/python3 /tmp/ployz-build-git/server.py >/dev/null
for _ in $(seq 1 50); do curl -fsS -u builder:build-secret-370 'https://${CORE}:9443/repo.git/info/refs?service=git-upload-pack' >/dev/null && exit 0; sleep 0.2; done
exit 1
SETUP_GIT
scp "${SSH_OPTS[@]}" "root@${CORE}:/tmp/ployz-build-git/server.crt" "$git_fixture_dir/server.crt" >/dev/null
scp "${SSH_OPTS[@]}" "$git_fixture_dir/server.crt" "root@${EDGE}:/usr/local/share/ca-certificates/ployz-build-git.crt" >/dev/null
remote "$EDGE" 'update-ca-certificates >/dev/null'
build_commit=$(core 'cat /tmp/ployz-build-git/commit')

assert_build_evidence() {
  local operation_id=$1
  local expected_adapter=$2
  local first second first_index second_index
  first=$(core "ployz ops watch '${operation_id}' --json")
  second=$(core "ployz ops watch '${operation_id}' --json")
  [ "$first" = "$second" ] || { log "${operation_id} operation evidence was not stable"; exit 1; }
  first_index=$(BUILD_EVENTS="$first" python3 -c 'import json,os; events=[json.loads(line)["event"] for line in os.environ["BUILD_EVENTS"].splitlines() if line]; print(next(event["receipt"]["index_digest"] for event in events if event.get("event") == "build_completed"))')
  second_index=$(BUILD_EVENTS="$second" python3 -c 'import json,os; events=[json.loads(line)["event"] for line in os.environ["BUILD_EVENTS"].splitlines() if line]; print(next(event["receipt"]["index_digest"] for event in events if event.get("event") == "build_completed"))')
  [ -n "$first_index" ] && [ "$first_index" = "$second_index" ] || { log "${operation_id} logical index was empty or unstable"; exit 1; }
  BUILD_EVENTS="$first" BUILD_COMMIT="$build_commit" BUILD_ADAPTER="$expected_adapter" python3 - <<'PY'
import json, os
events = [json.loads(line)["event"] for line in os.environ["BUILD_EVENTS"].splitlines() if line]
verified = [event for event in events if event.get("event") == "build_commit_verified"]
assert any(event["commit"]["commit"] == os.environ["BUILD_COMMIT"] for event in verified)
completed = [event for event in events if event.get("event") == "build_completed"]
assert len(completed) == 1
platforms = completed[0]["receipt"]["platforms"]
assert {(item[0]["os"], item[0]["architecture"]) for item in platforms} == {("linux", "amd64"), ("linux", "arm64")}
submitted = [event for event in events if event.get("event") == "build_submitted"]
assert len(submitted) == 1 and submitted[0]["adapter"]["adapter"] == os.environ["BUILD_ADAPTER"]
PY
}

log "building authenticated exact SHA for amd64 and arm64 with Dockerfile"
core "set -a; . /tmp/ployz-build-git/secret.env; set +a; timeout 30m ployz build submit --git 'https://${CORE}:9443/repo.git' --commit '${build_commit}' --git-username builder --git-secret-env PLOYZ_BUILD_GIT_SECRET --subdir dockerfile --platform linux/amd64 --platform linux/arm64 --dockerfile Dockerfile --operation-id op_real_host_build_dockerfile"
assert_build_evidence op_real_host_build_dockerfile dockerfile

log "building authenticated exact SHA for amd64 and arm64 with Railpack"
core "set -a; . /tmp/ployz-build-git/secret.env; set +a; timeout 30m ployz build submit --git 'https://${CORE}:9443/repo.git' --commit '${build_commit}' --git-username builder --git-secret-env PLOYZ_BUILD_GIT_SECRET --subdir railpack --platform linux/amd64 --platform linux/arm64 --railpack --cache-scope real-host-railpack --operation-id op_real_host_build_railpack"
assert_build_evidence op_real_host_build_railpack railpack

log "cancelling a blocking authenticated build and checking cleanup evidence"
core "set -a; . /tmp/ployz-build-git/secret.env; set +a; ployz build submit --git 'https://${CORE}:9443/repo.git' --commit '${build_commit}' --git-username builder --git-secret-env PLOYZ_BUILD_GIT_SECRET --subdir slow --platform linux/amd64 --dockerfile Dockerfile --operation-id op_real_host_build_cancel --detach"
for _ in $(seq 1 180); do
  if core 'ployz ops status op_real_host_build_cancel' | grep -q 'state building'; then break; fi
  sleep 1
done
core 'ployz ops status op_real_host_build_cancel' | grep -q 'state building'
core 'ployz build cancel op_real_host_build_cancel --reason "real-host cancellation proof"'
cancel_events=$(core 'ployz ops watch op_real_host_build_cancel --json' || true)
CANCEL_EVENTS="$cancel_events" python3 - <<'PY'
import json, os
events = [json.loads(line)["event"] for line in os.environ["CANCEL_EVENTS"].splitlines() if line]
cancelled = [event for event in events if event.get("event") == "build_cancelled"]
assert len(cancelled) == 1 and cancelled[0]["cleanup"]["kind"] == "completed"
PY

log "checking keeper-managed firewall rules"
for port in 4222/tcp 80/tcp 443/tcp 51820/udp; do
  core "firewall-cmd --quiet --query-port=${port} && firewall-cmd --permanent --quiet --query-port=${port}"
done
for port in 80/tcp 443/tcp 51820/udp; do
  remote "$EDGE" "ufw status | awk '{print \$1}' | grep -Fx '${port}' >/dev/null"
done

log "deploying image-based Compose app (2 replicas, managed HTTPS URL)"
core 'cat > /tmp/ployz-acceptance.yml <<"YAML"
name: acceptance
services:
  web:
    image: nginx:alpine
    deploy:
      replicas: 2
    x-ports:
      - auto:web:80
YAML'
t0=$(ts)
deploy_output=$(core 'timeout 15m ployz deploy -f /tmp/ployz-acceptance.yml')
printf '%s\n' "$deploy_output"
log "TIMING deploy=$(( $(ts)-t0 ))s"

hostname=$(printf '%s\n' "$deploy_output" | managed_host)
service_output=$(core 'ployz service inspect web')
printf '%s\n' "$service_output"
if [ -z "$hostname" ]; then
  hostname=$(printf '%s\n' "$service_output" | managed_host)
fi
[ -n "$hostname" ] || { log "managed public HTTPS URL not found"; exit 1; }
core_container=$(printf '%s\n' "$service_output" | awk '/^container .* machine ployz-core / {print $2; exit}')
edge_container=$(printf '%s\n' "$service_output" | awk '/^container .* machine ployz-edge / {print $2; exit}')
[ -n "$core_container" ] || { log "no web replica placed on ployz-core"; exit 1; }
[ -n "$edge_container" ] || { log "no web replica placed on ployz-edge"; exit 1; }

log "public HTTPS URL: https://${hostname}"
code=$(curl --connect-timeout 20 --max-time 60 -sS -o /dev/null -w '%{http_code}' "https://${hostname}/")
log "  public DNS route -> HTTPS ${code}"
[ "$code" = 200 ] || { log "public URL returned ${code}"; exit 1; }

log "cross-machine routing with each gateway's local replica stopped"
for pair in "$CORE:$core_container" "$EDGE:$edge_container"; do
  host=${pair%%:*}
  container=${pair#*:}
  stopped_host=$host
  stopped_container=$container
  remote "$host" "docker stop '${container}' >/dev/null"
  if code=$(remote "$host" "curl --retry 10 --retry-delay 1 --retry-all-errors --connect-timeout 5 --max-time 30 --resolve '${hostname}:443:127.0.0.1' -fsS -o /dev/null -w '%{http_code}' 'https://${hostname}/'"); then
    route_ok=1
  else
    route_ok=0
  fi
  remote "$host" "docker start '${container}' >/dev/null"
  stopped_host=
  stopped_container=
  log "  gateway ${host} -> remote replica -> HTTPS ${code:-failed}"
  [ "$route_ok" = 1 ] && [ "$code" = 200 ] || { log "cross-machine route failed via ${host}"; exit 1; }
done

log "restart invisibility: probing the core gateway while ployzd-control restarts"
probe_dir=$(mktemp -d)
(
  while [ ! -e "$probe_dir/stop" ]; do
    if curl --connect-timeout 2 --max-time 5 --resolve "${hostname}:443:${CORE}" -fsS -o /dev/null "https://${hostname}/"; then
      touch "$probe_dir/ready"
    else
      touch "$probe_dir/failed"
    fi
    sleep 0.1
  done
) &
probe_pid=$!
for _ in $(seq 1 100); do
  [ -e "$probe_dir/ready" ] && break
  sleep 0.1
done
[ -e "$probe_dir/ready" ] || { log "restart probe did not become ready"; exit 1; }
[ ! -e "$probe_dir/failed" ] || { log "route failed before restart"; exit 1; }
if core 'systemctl restart ployzd-control && sleep 3 && systemctl is-active --quiet ployzd-control'; then
  restart_ok=1
else
  restart_ok=0
fi
touch "$probe_dir/stop"
wait "$probe_pid"
probe_pid=
[ "$restart_ok" = 1 ] || { rm -rf "$probe_dir"; log "ployzd-control restart failed"; exit 1; }
[ ! -e "$probe_dir/failed" ] || { rm -rf "$probe_dir"; log "route failed during restart"; exit 1; }
rm -rf "$probe_dir"
probe_dir=
log "  continuous HTTPS probe saw no interruption"

log "ACCEPTANCE PASSED: mixed-arch + firewalld/UFW + public HTTPS"
