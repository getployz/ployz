#!/usr/bin/env bash
# Fresh-host acceptance: Rocky 9 amd64 core + Ubuntu 24.04 arm64 edge.
# See docs/operations/real-host-acceptance.md before running.
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)

ZFS_CERTIFY=${PLOYZ_REAL_HOST_ZFS_CERTIFY:-0}
ZFS_EVIDENCE_DIR=${PLOYZ_REAL_HOST_EVIDENCE_DIR:-}
EXPECTED_RELEASE_TAG=${PLOYZ_EXPECTED_RELEASE_TAG:-}
EXPECTED_RUNTIME_SHA=${PLOYZ_EXPECTED_RUNTIME_SHA:-}
MINIMUM_ZFS_RUNTIME_SHA=2f754ab5cff785fd67cf4c83231f4025ec6ad8ee

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

if [ "$ZFS_CERTIFY" != 0 ] && [ "$ZFS_CERTIFY" != 1 ]; then
  log "PLOYZ_REAL_HOST_ZFS_CERTIFY must be 0 or 1"
  exit 1
fi
if [ "$ZFS_CERTIFY" = 1 ]; then
  [ -n "$ZFS_EVIDENCE_DIR" ] || {
    log "PLOYZ_REAL_HOST_EVIDENCE_DIR is required for ZFS certification"
    exit 1
  }
  [[ "$ZFS_EVIDENCE_DIR" = /* ]] || {
    log "PLOYZ_REAL_HOST_EVIDENCE_DIR must be an absolute path"
    exit 1
  }
  [ -n "$EXPECTED_RELEASE_TAG" ] || {
    log "PLOYZ_EXPECTED_RELEASE_TAG is required for ZFS certification"
    exit 1
  }
  [[ "$EXPECTED_RUNTIME_SHA" =~ ^[0-9a-f]{40}$ ]] || {
    log "PLOYZ_EXPECTED_RUNTIME_SHA must be a full lowercase Git SHA"
    exit 1
  }
  [ -z "$(git -C "$REPO_ROOT" status --porcelain)" ] || {
    log "the local harness worktree must be clean for ZFS certification"
    exit 1
  }
  local_tag_sha=$(git -C "$REPO_ROOT" rev-parse --verify "${EXPECTED_RELEASE_TAG}^{commit}" 2>/dev/null) || {
    log "expected release tag ${EXPECTED_RELEASE_TAG} is not available locally"
    exit 1
  }
  [ "$local_tag_sha" = "$EXPECTED_RUNTIME_SHA" ] || {
    log "expected release tag resolves to ${local_tag_sha}, not ${EXPECTED_RUNTIME_SHA}"
    exit 1
  }
  git -C "$REPO_ROOT" merge-base --is-ancestor "$MINIMUM_ZFS_RUNTIME_SHA" "$EXPECTED_RUNTIME_SHA" || {
    log "runtime ${EXPECTED_RUNTIME_SHA} does not contain required ZFS testimony commit ${MINIMUM_ZFS_RUNTIME_SHA}"
    exit 1
  }
  mkdir -p "$ZFS_EVIDENCE_DIR"
  [ -z "$(find "$ZFS_EVIDENCE_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ] || {
    log "ZFS evidence directory must be empty: ${ZFS_EVIDENCE_DIR}"
    exit 1
  }
  harness_sha=$(git -C "$REPO_ROOT" rev-parse HEAD)
  {
    printf 'started_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'core=%s\nedge=%s\n' "$CORE" "$EDGE"
    printf 'release_tag=%s\nruntime_sha=%s\n' "$EXPECTED_RELEASE_TAG" "$EXPECTED_RUNTIME_SHA"
    printf 'harness_sha=%s\nminimum_runtime_sha=%s\n' "$harness_sha" "$MINIMUM_ZFS_RUNTIME_SHA"
  } > "$ZFS_EVIDENCE_DIR/metadata.env"
  exec > >(tee -a "$ZFS_EVIDENCE_DIR/transcript.log") 2>&1
fi

stopped_host=
stopped_container=
probe_dir=
probe_pid=
git_fixture_dir=
zfs_recovery_message=
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
  if [ -n "$zfs_recovery_message" ]; then
    printf '\n%s\n' "$zfs_recovery_message" >&2
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
if [ "$ZFS_CERTIFY" = 1 ]; then
  core 'source /etc/os-release; [ "$ID" = rocky ] && [[ "$VERSION_ID" = 9* ]]' || {
    log "ZFS certification requires Rocky Linux 9 on the core"; exit 1;
  }
  remote "$EDGE" 'source /etc/os-release; [ "$ID" = ubuntu ] && [ "$VERSION_ID" = 24.04 ]' || {
    log "ZFS certification requires Ubuntu 24.04 on the edge"; exit 1;
  }
  core_os=$(core 'source /etc/os-release; printf "%s %s" "$ID" "$VERSION_ID"')
  edge_os=$(remote "$EDGE" 'source /etc/os-release; printf "%s %s" "$ID" "$VERSION_ID"')
  core_kernel=$(core 'uname -r')
  edge_kernel=$(remote "$EDGE" 'uname -r')
  printf 'core_os=%s\nedge_os=%s\ncore_arch=%s\nedge_arch=%s\ncore_kernel=%s\nedge_kernel=%s\n' \
    "$core_os" "$edge_os" "$core_arch" "$edge_arch" "$core_kernel" "$edge_kernel" >> "$ZFS_EVIDENCE_DIR/metadata.env"
fi

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
if [ "$ZFS_CERTIFY" = 1 ]; then
  curl --connect-timeout 20 --max-time 120 -fsSL https://ployz.sh/channels/alpha.env \
    -o "$ZFS_EVIDENCE_DIR/live-alpha.env"
  live_alpha_tag=$(awk -F= '$1 == "PLOYZ_RELEASE_TAG" { print substr($0, index($0, "=") + 1); exit }' "$ZFS_EVIDENCE_DIR/live-alpha.env")
  [ "$live_alpha_tag" = "$EXPECTED_RELEASE_TAG" ] || {
    log "live alpha channel points to ${live_alpha_tag:-missing}, not ${EXPECTED_RELEASE_TAG}"
    exit 1
  }
  installed_tag=$(core "awk -F= '\$1 == \"PLOYZ_RELEASE_TAG\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  installed_manifest=$(core "awk -F= '\$1 == \"PLOYZ_RELEASE_MANIFEST_URL\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  [ "$installed_tag" = "$EXPECTED_RELEASE_TAG" ] || {
    log "installed release tag ${installed_tag:-missing} does not match ${EXPECTED_RELEASE_TAG}"
    exit 1
  }
  case "$installed_manifest" in
    "https://github.com/getployz/ployz/releases/download/${EXPECTED_RELEASE_TAG}/"*) ;;
    *) log "installed manifest is not pinned beneath immutable release tag ${EXPECTED_RELEASE_TAG}"; exit 1 ;;
  esac
  printf 'live_alpha_tag=%s\ncore_installed_tag=%s\ncore_installed_manifest=%s\n' \
    "$live_alpha_tag" "$installed_tag" "$installed_manifest" >> "$ZFS_EVIDENCE_DIR/metadata.env"
fi

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
if [ "$ZFS_CERTIFY" = 1 ]; then
  edge_installed_tag=$(remote "$EDGE" "awk -F= '\$1 == \"PLOYZ_RELEASE_TAG\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  edge_installed_manifest=$(remote "$EDGE" "awk -F= '\$1 == \"PLOYZ_RELEASE_MANIFEST_URL\" { print substr(\$0, index(\$0, \"=\") + 1); exit }' /etc/ployz/release.env")
  [ "$edge_installed_tag" = "$EXPECTED_RELEASE_TAG" ] || {
    log "edge installed release tag ${edge_installed_tag:-missing} does not match ${EXPECTED_RELEASE_TAG}"
    exit 1
  }
  case "$edge_installed_manifest" in
    "https://github.com/getployz/ployz/releases/download/${EXPECTED_RELEASE_TAG}/"*) ;;
    *) log "edge installed manifest is not pinned beneath immutable release tag ${EXPECTED_RELEASE_TAG}"; exit 1 ;;
  esac
  printf 'edge_installed_tag=%s\nedge_installed_manifest=%s\n' \
    "$edge_installed_tag" "$edge_installed_manifest" >> "$ZFS_EVIDENCE_DIR/metadata.env"
fi

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

wait_for_core_reboot() {
  local prior_boot_id=$1
  local saw_disconnect=0
  local current_boot_id=

  for _ in $(seq 1 60); do
    if ! core true 2>/dev/null; then
      saw_disconnect=1
      break
    fi
    sleep 2
  done
  [ "$saw_disconnect" = 1 ] || { log "core did not disconnect for reboot"; return 1; }

  for _ in $(seq 1 180); do
    if current_boot_id=$(core 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null) \
      && [ "$current_boot_id" != "$prior_boot_id" ]; then
      printf '%s\n' "$current_boot_id"
      return 0
    fi
    sleep 3
  done
  log "core did not return with a new boot id within 9 minutes"
  return 1
}

reboot_core() {
  local prior_boot_id=$1
  core "nohup sh -c 'sleep 1; systemctl reboot' >/dev/null 2>&1 &" || true
  wait_for_core_reboot "$prior_boot_id"
}

wait_for_core_api() {
  for _ in $(seq 1 120); do
    if core 'systemctl is-active --quiet ployzd-control && timeout 10s ployz machine list >/dev/null' 2>/dev/null; then
      return 0
    fi
    sleep 3
  done
  log "Control/API did not return within 6 minutes"
  return 1
}

zfs_phase() {
  local number=$1
  local name=$2
  shift 2
  local started elapsed status phase_log
  started=$(ts)
  log "ZFS PHASE ${number}: ${name}"
  phase_log="$ZFS_EVIDENCE_DIR/${number}-${name}.log"
  if "$@" > "$phase_log" 2>&1; then
    status=0
  else
    status=$?
  fi
  cat "$phase_log"
  elapsed=$(( $(ts) - started ))
  printf 'phase_%s_%s_seconds=%s\n' "$number" "${name//-/_}" "$elapsed" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  log "TIMING zfs-${name}=${elapsed}s"
  return "$status"
}

zfs_storage_prepare() {
  storage_prepare_output=$(core "timeout 2m ployz machine storage-prepare '${core_machine_id}' --detach")
  printf '%s\n' "$storage_prepare_output"
  storage_operation_id=$(printf '%s\n' "$storage_prepare_output" | awk '/^operation / { print $2; exit }')
  [ -n "$storage_operation_id" ] || { log "storage preparation did not report an operation id"; return 1; }
  core "timeout 20s ployz ops status '${storage_operation_id}'"
  core "timeout 20m ployz ops watch '${storage_operation_id}' --json"
  machine_storage=$(core "timeout 20s ployz machine inspect '${core_machine_id}'")
  printf '%s\n' "$machine_storage"
  pool=$(printf '%s\n' "$machine_storage" | sed -n 's/^storage ready pool=//p' | head -1)
  [ -n "$pool" ] || { log "storage preparation did not report a ready pool"; return 1; }
}

zfs_deploy_database() {
  core "timeout 15m ployz volume create '${zfs_namespace}' '${zfs_volume}' --machine '${core_machine_id}' --max-size 2G"
  core "cat > '${remote_compose}' <<'YAML'
name: ${zfs_namespace}
services:
  ${zfs_service}:
    image: postgres:15-alpine
    restart: unless-stopped
    environment:
      POSTGRES_DB: ployzcert
      POSTGRES_USER: ployzcert
      POSTGRES_PASSWORD: ployzcert
    volumes:
      - ${zfs_volume}:/var/lib/postgresql/data
    healthcheck:
      test: [\"CMD-SHELL\", \"pg_isready -U ployzcert -d ployzcert\"]
      interval: 2s
      timeout: 2s
      retries: 60
volumes:
  ${zfs_volume}:
    x-ployz:
      max-size: 2G
YAML"
  core "timeout 20m ployz deploy -f '${remote_compose}'"
  volume_output=$(core 'timeout 20s ployz volume list')
  printf '%s\n' "$volume_output"
  volume_row=$(printf '%s\n' "$volume_output" | awk -v ns="$zfs_namespace" -v volume="$zfs_volume" '$1 == ns && $2 == volume { print; exit }')
  [ -n "$volume_row" ] || { log "provisioned volume was not listed"; return 1; }
  [ "$(printf '%s\n' "$volume_row" | awk '{print $3}')" = "$core_machine_id" ] || {
    log "provisioned volume was not pinned to the Rocky core"; return 1;
  }
  [ "$(printf '%s\n' "$volume_row" | awk '{print $4}')" = provisioned ] || {
    log "volume is not provisioned"; return 1;
  }
  dataset=$(printf '%s\n' "$volume_row" | awk '{print $5}')
  if [ -z "$dataset" ] || [ "$dataset" = - ]; then
    log "provisioned dataset identity is missing"
    return 1
  fi

  db_container=$(core "docker ps -q --filter 'label=plz.namespace_id=${zfs_namespace}' --filter 'label=plz.service_id=${zfs_service}' | head -1")
  [ -n "$db_container" ] || { log "database container is not running on the core"; return 1; }
  baseline_db_container=$db_container
  core "timeout 30s docker exec '${db_container}' psql -U ployzcert -d ployzcert -v ON_ERROR_STOP=1 -c \"CREATE TABLE certification (marker text PRIMARY KEY); INSERT INTO certification VALUES ('${row_marker}');\""
}

zfs_capture_baseline() {
  pool_guid=$(core "zpool get -H -o value guid '${pool}'")
  dataset_guid=$(core "zfs get -H -o value guid '${dataset}'")
  dataset_mountpoint=$(core "zfs get -H -o value mountpoint '${dataset}'")
  docker_volume_name=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Name}}{{end}}{{end}}' '${db_container}'")
  docker_volume_source=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Source}}{{end}}{{end}}' '${db_container}'")
  bind_device=$(core "docker volume inspect --format '{{index .Options \"device\"}}' '${docker_volume_name}'")
  if [ -z "$pool_guid" ] || [ -z "$dataset_guid" ] || [ -z "$dataset_mountpoint" ] \
    || [ -z "$docker_volume_name" ] || [ -z "$docker_volume_source" ] || [ -z "$bind_device" ]; then
    log "baseline storage identity is incomplete"
    return 1
  fi
  [ "$bind_device" = "$dataset_mountpoint" ] || { log "Docker volume is not bound to the dataset mountpoint"; return 1; }
  core "mountpoint -q '${dataset_mountpoint}' && test -d '${docker_volume_source}'"
  core "timeout 30s docker exec '${db_container}' psql -At -U ployzcert -d ployzcert -c \"SELECT marker FROM certification WHERE marker='${row_marker}'\"" | grep -Fx "$row_marker"
  {
    printf 'namespace=%s\nvolume=%s\nservice=%s\n' "$zfs_namespace" "$zfs_volume" "$zfs_service"
    printf 'machine_id=%s\nstorage_operation_id=%s\npool=%s\ndataset=%s\n' "$core_machine_id" "$storage_operation_id" "$pool" "$dataset"
    printf 'pool_guid=%s\ndataset_guid=%s\ndataset_mountpoint=%s\n' "$pool_guid" "$dataset_guid" "$dataset_mountpoint"
    printf 'docker_volume_name=%s\ndocker_volume_source=%s\nbind_device=%s\nbaseline_container=%s\n' \
      "$docker_volume_name" "$docker_volume_source" "$bind_device" "$baseline_db_container"
    printf 'row_marker=%s\n' "$row_marker"
  } >> "$ZFS_EVIDENCE_DIR/metadata.env"
}

zfs_verify_reboot_recovery() {
  wait_for_core_api
  core "zfs_time=\$(systemctl show zfs.target -p ActiveEnterTimestampMonotonic --value); docker_time=\$(systemctl show docker.service -p ActiveEnterTimestampMonotonic --value); after=\$(systemctl show docker.service -p After --value); printf 'zfs_active_us=%s\\ndocker_active_us=%s\\ndocker_after=%s\\n' \"\$zfs_time\" \"\$docker_time\" \"\$after\"; [ \"\$zfs_time\" -gt 0 ] && [ \"\$docker_time\" -gt 0 ] && [ \"\$zfs_time\" -le \"\$docker_time\" ] && printf '%s\\n' \"\$after\" | tr ' ' '\\n' | grep -Fx zfs.target >/dev/null"
  core "timeout 3m sh -c 'until zpool list -H -o guid \"${pool}\" | grep -Fx \"${pool_guid}\"; do sleep 2; done'"
  [ "$(core "zfs get -H -o value guid '${dataset}'")" = "$dataset_guid" ] || {
    log "dataset GUID changed across reboot"; return 1;
  }
  core "mountpoint -q '${bind_device}' && test -d '${docker_volume_source}'"
  recovered_container=
  for _ in $(seq 1 120); do
    recovered_container=$(core "docker ps -q --filter 'label=plz.namespace_id=${zfs_namespace}' --filter 'label=plz.service_id=${zfs_service}' | head -1")
    [ -n "$recovered_container" ] && break
    sleep 2
  done
  [ -n "$recovered_container" ] || { log "database container did not return after reboot"; return 1; }
  [ "$recovered_container" = "$baseline_db_container" ] || { log "a replacement database container returned after reboot"; return 1; }
  recovered_volume_name=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Name}}{{end}}{{end}}' '${recovered_container}'")
  recovered_source=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Source}}{{end}}{{end}}' '${recovered_container}'")
  recovered_device=$(core "docker volume inspect --format '{{index .Options \"device\"}}' '${recovered_volume_name}'")
  if [ "$recovered_volume_name" != "$docker_volume_name" ] \
    || [ "$recovered_source" != "$docker_volume_source" ] \
    || [ "$recovered_device" != "$bind_device" ]; then
    log "database returned on a different Docker volume or bind device"
    return 1
  fi
  core "timeout 30s docker exec '${recovered_container}' psql -At -U ployzcert -d ployzcert -c \"SELECT marker FROM certification WHERE marker='${row_marker}'\"" | grep -Fx "$row_marker"
  db_container=$recovered_container
}

zfs_quarantine_module() {
  recovery_root="/root/ployz-zfs-recovery-${zfs_suffix}"
  module_path=$(core 'modinfo -n zfs')
  [[ "$module_path" = /lib/modules/* ]] || { log "refusing unexpected ZFS module path ${module_path}"; return 1; }
  module_backup="${recovery_root}/module/${module_path#/}"
  quarantined_module="${recovery_root}/quarantined/$(basename "$module_path")"
  core "command -v lsinitrd >/dev/null && test -f '${module_path}' && test ! -e '${recovery_root}'"
  initramfs="/boot/initramfs-$(core 'uname -r').img"
  printf '05 exact module path=%s backup=%s quarantine=%s initramfs=%s\n' \
    "$module_path" "$module_backup" "$quarantined_module" "$initramfs" >> "$ZFS_EVIDENCE_DIR/commands.log"
  core "test -f '${initramfs}' && ! lsinitrd '${initramfs}' | grep -Eq '(^|/)(zfs|spl)\\.ko(\\.(xz|zst|gz))?($|[[:space:]])'"
  core "install -d -m 0700 '${recovery_root}/module/$(dirname "${module_path#/}")' '${recovery_root}/quarantined'; cp -a '${module_path}' '${module_backup}'; module_sha=\$(sha256sum '${module_path}' | awk '{print \$1}'); { printf 'module_path=%s\\nbackup_path=%s\\nquarantine_path=%s\\n' '${module_path}' '${module_backup}' '${quarantined_module}'; printf 'module_sha256=%s\\nmodule_mode=%s\\nmodule_uid=%s\\nmodule_gid=%s\\n' \"\$module_sha\" \"\$(stat -c %a '${module_path}')\" \"\$(stat -c %u '${module_path}')\" \"\$(stat -c %g '${module_path}')\"; printf 'initramfs=%s\\nkernel=%s\\n' '${initramfs}' \"\$(uname -r)\"; } > '${recovery_root}/zfs-module-recovery.env'; chmod 0600 '${recovery_root}/zfs-module-recovery.env'; mv '${module_path}' '${quarantined_module}'; depmod -a; ! modinfo zfs >/dev/null 2>&1"
  scp "${SSH_OPTS[@]}" "root@${CORE}:${recovery_root}/zfs-module-recovery.env" "$ZFS_EVIDENCE_DIR/zfs-module-recovery.env" >/dev/null
  cat > "$ZFS_EVIDENCE_DIR/recovery.txt" <<EOF
ZFS module recovery on root@${CORE}:
  test "\$(sha256sum '${module_backup}' | awk '{print \$1}')" = "\$(awk -F= '\$1 == \"module_sha256\" {print \$2}' '${recovery_root}/zfs-module-recovery.env')"
  cp -a '${module_backup}' '${module_path}'
  test "\$(sha256sum '${module_path}' | awk '{print \$1}')" = "\$(awk -F= '\$1 == \"module_sha256\" {print \$2}' '${recovery_root}/zfs-module-recovery.env')"
  depmod -a
  reboot
No pool, dataset, volume, container data, or host is destroyed by this harness.
EOF
  zfs_recovery_message=$(cat "$ZFS_EVIDENCE_DIR/recovery.txt")
  cat "$ZFS_EVIDENCE_DIR/zfs-module-recovery.env"
  cat "$ZFS_EVIDENCE_DIR/recovery.txt"
}

zfs_verify_module_failure() {
  wait_for_core_api
  core "! zpool list '${pool}' >/dev/null 2>&1 && ! zfs list '${dataset}' >/dev/null 2>&1"
  core "test ! -e '${bind_device}'"
  failure_containers=$(core "docker ps -aq --filter 'label=plz.namespace_id=${zfs_namespace}' --filter 'label=plz.service_id=${zfs_service}'")
  if [ "$(printf '%s\n' "$failure_containers" | sed '/^$/d' | wc -l)" -ne 1 ] \
    || [ "$failure_containers" != "$baseline_db_container" ]; then
    log "failure evidence does not contain exactly the baseline database container"
    return 1
  fi
  for container in $failure_containers; do
    [ "$(core "docker inspect --format '{{.State.Running}}' '${container}'")" = false ] || {
      log "database container ${container} is running without ZFS"; return 1;
    }
    failure_volume_name=$(core "docker inspect --format '{{range .Mounts}}{{if eq .Destination \"/var/lib/postgresql/data\"}}{{.Name}}{{end}}{{end}}' '${container}'")
    if [ "$container" != "$baseline_db_container" ] || [ "$failure_volume_name" != "$docker_volume_name" ]; then
      log "database failure evidence names a replacement container or volume"
      return 1
    fi
  done
  if start_failure=$(core "docker start '${baseline_db_container}'" 2>&1); then
    log "database container started without its ZFS bind device"; return 1
  fi
  state_error=$(core "docker inspect --format '{{.State.Error}}' '${baseline_db_container}'")
  printf 'docker-start-error:\n%s\ndocker-state-error:\n%s\n' "$start_failure" "$state_error"
  printf '%s\n%s\n' "$start_failure" "$state_error" | grep -Eqi 'mount|bind|no such file|does not exist' || {
    log "Docker start failed without mount/bind evidence"; return 1;
  }
  [ "$(core "docker inspect --format '{{.State.Running}}' '${baseline_db_container}'")" = false ] \
    || { log "database container became running after failed start"; return 1; }
  core "journalctl -b -u docker.service --no-pager -n 100"
  core 'systemctl is-active --quiet ployzd-control && timeout 10s ployz ops list >/dev/null'
  for _ in $(seq 1 120); do
    failure_testimony=$(core "timeout 20s ployz machine inspect '${core_machine_id}'")
    if printf '%s\n' "$failure_testimony" | grep -Fx 'storage unavailable zfs-module-missing' >/dev/null \
      && printf '%s\n' "$failure_testimony" | grep -Fx "storage-alarms ${zfs_namespace}/${zfs_volume}:zfs-module-missing" >/dev/null; then
      printf '%s\n' "$failure_testimony"
      return 0
    fi
    sleep 3
  done
  printf '%s\n' "$failure_testimony"
  log "typed ZFS module failure and exact stranded pin did not converge"
  return 1
}

zfs_restore_module() {
  core "test -f '${quarantined_module}'; expected=\$(awk -F= '\$1 == \"module_sha256\" {print \$2}' '${recovery_root}/zfs-module-recovery.env'); expected_mode=\$(awk -F= '\$1 == \"module_mode\" {print \$2}' '${recovery_root}/zfs-module-recovery.env'); expected_uid=\$(awk -F= '\$1 == \"module_uid\" {print \$2}' '${recovery_root}/zfs-module-recovery.env'); expected_gid=\$(awk -F= '\$1 == \"module_gid\" {print \$2}' '${recovery_root}/zfs-module-recovery.env'); [ \"\$(sha256sum '${module_backup}' | awk '{print \$1}')\" = \"\$expected\" ]; cp -a '${module_backup}' '${module_path}'; [ \"\$(sha256sum '${module_path}' | awk '{print \$1}')\" = \"\$expected\" ] && [ \"\$(stat -c %a '${module_path}')\" = \"\$expected_mode\" ] && [ \"\$(stat -c %u '${module_path}')\" = \"\$expected_uid\" ] && [ \"\$(stat -c %g '${module_path}')\" = \"\$expected_gid\" ]; depmod -a; modinfo zfs >/dev/null"
}

zfs_verify_alarm_cleared() {
  wait_for_core_api
  zfs_verify_reboot_recovery
  for _ in $(seq 1 120); do
    restored_testimony=$(core "timeout 20s ployz machine inspect '${core_machine_id}'")
    if printf '%s\n' "$restored_testimony" | grep -F "storage ready pool=${pool}" >/dev/null \
      && ! printf '%s\n' "$restored_testimony" | grep -F "${zfs_namespace}/${zfs_volume}:" >/dev/null; then
      printf '%s\n' "$restored_testimony"
      return 0
    fi
    sleep 3
  done
  printf '%s\n' "$restored_testimony"
  log "stranded-volume alarm did not clear"
  return 1
}

run_zfs_real_host_certification() {
  local zfs_started
  zfs_started=$(ts)
  zfs_suffix="$(date -u +%m%d%H%M)-$$"
  zfs_namespace="zfs${zfs_suffix//-/}"
  zfs_volume="data${zfs_suffix//-/}"
  zfs_service="db${zfs_suffix//-/}"
  row_marker="ployz-${zfs_suffix}-$(printf '%s' "${CORE}-${EDGE}-${zfs_started}" | sha256sum | cut -c1-12)"
  remote_compose="/tmp/ployz-zfs-${zfs_suffix}.yml"
  core_machine_id=$(core "ployz machine list | awk '\$2 == \"ployz-core\" { print \$1; exit }'")
  [ -n "$core_machine_id" ] || { log "could not resolve the Rocky core machine id"; return 1; }
  install -m 0444 "$REPO_ROOT/scripts/real-host-acceptance.sh" "$ZFS_EVIDENCE_DIR/sealed-harness.sh"
  cat > "$ZFS_EVIDENCE_DIR/commands.log" <<EOF
Harness source: sealed-harness.sh (${harness_sha})
01 timeout 2m ployz machine storage-prepare ${core_machine_id} --detach; timeout 20m ployz ops watch OPERATION --json
02 ployz volume create ${zfs_namespace} ${zfs_volume} --machine ${core_machine_id} --max-size 2G; ployz deploy -f ${remote_compose}
03 zpool/zfs GUID and mountpoint; docker inspect container/volume; PostgreSQL row query
04 systemctl reboot; systemd zfs.target-before-docker timestamps; same pool/dataset/container/volume/bind/row
05 modinfo/lsinitrd guard; root-only copy+metadata+checksum; move module; depmod; modinfo absence
06 systemctl reboot; absent pool/dataset/bind device; stopped same container; Control/API and typed stranded-pin testimony
07 verify backup checksum; restore exact module path; depmod; modinfo
08 systemctl reboot; same pool/dataset/container/volume/bind/row; alarm clear
09 systemctl reboot; repeat full recovery and alarm-clear proof
Commands containing fixture credentials are intentionally redacted; their bounded outcomes are in the numbered logs.
EOF

  zfs_phase 01 storage-prepare zfs_storage_prepare
  zfs_phase 02 database-deploy zfs_deploy_database
  zfs_phase 03 baseline-identity zfs_capture_baseline

  initial_boot_id=$(core 'cat /proc/sys/kernel/random/boot_id')
  reboot_started=$(ts)
  baseline_reboot_id=$(reboot_core "$initial_boot_id")
  printf 'initial_boot_id=%s\nbaseline_reboot_id=%s\nbaseline_reboot_seconds=%s\n' \
    "$initial_boot_id" "$baseline_reboot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 04 baseline-reboot zfs_verify_reboot_recovery

  zfs_phase 05 module-quarantine zfs_quarantine_module
  reboot_started=$(ts)
  quarantine_boot_id=$(reboot_core "$baseline_reboot_id")
  printf 'quarantine_boot_id=%s\nquarantine_reboot_seconds=%s\n' \
    "$quarantine_boot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 06 module-failure zfs_verify_module_failure

  zfs_phase 07 module-restore zfs_restore_module
  reboot_started=$(ts)
  restore_boot_id=$(reboot_core "$quarantine_boot_id")
  printf 'restore_boot_id=%s\nrestore_reboot_seconds=%s\n' \
    "$restore_boot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 08 recovery zfs_verify_alarm_cleared

  reboot_started=$(ts)
  final_boot_id=$(reboot_core "$restore_boot_id")
  printf 'final_boot_id=%s\nfinal_reboot_seconds=%s\n' \
    "$final_boot_id" "$(( $(ts) - reboot_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  zfs_phase 09 final-reboot zfs_verify_alarm_cleared
  printf 'completed_utc=%s\ntotal_seconds=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(( $(ts) - zfs_started ))" >> "$ZFS_EVIDENCE_DIR/metadata.env"
  checksum_tmp=$(mktemp)
  (
    cd "$ZFS_EVIDENCE_DIR"
    find . -maxdepth 1 -type f ! -name transcript.log ! -name sha256sums -print0 \
      | sort -z | xargs -0 sha256sum > "$checksum_tmp"
  )
  mv "$checksum_tmp" "$ZFS_EVIDENCE_DIR/sha256sums"
  log "ZFS REAL-HOST CERTIFICATION PASSED"
}

if [ "$ZFS_CERTIFY" = 1 ]; then
  run_zfs_real_host_certification
fi

log "ACCEPTANCE PASSED: mixed-arch + firewalld/UFW + public HTTPS"
