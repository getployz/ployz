#!/usr/bin/env bash
# Fresh-host acceptance: Rocky 9 amd64 core + Ubuntu 24.04 arm64 edge.
# See docs/operations/real-host-acceptance.md before running.
set -euo pipefail

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
cleanup() {
  if [ -n "$probe_pid" ]; then
    touch "$probe_dir/stop"
    wait "$probe_pid" 2>/dev/null || true
  fi
  if [ -n "$stopped_container" ]; then
    remote "$stopped_host" "docker start '${stopped_container}' >/dev/null" || true
  fi
  [ -z "$probe_dir" ] || rm -rf "$probe_dir"
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
core "timeout 15m ployz machine init root@${CORE} --name ployz-core --public-ip ${CORE} --public-url auto"
log "TIMING machine-init=$(( $(ts)-t0 ))s"

core_key=$(core 'cat ~/.ssh/id_ed25519.pub')
remote "$EDGE" "grep -qF '${core_key}' ~/.ssh/authorized_keys 2>/dev/null || echo '${core_key}' >> ~/.ssh/authorized_keys"
core "ssh-keyscan -T 8 ${EDGE} >> ~/.ssh/known_hosts 2>/dev/null"

log "machine add (arm64 Ubuntu edge)"
t0=$(ts)
core "timeout 15m ployz machine add root@${EDGE} --name ployz-edge"
log "TIMING machine-add=$(( $(ts)-t0 ))s"
core 'ployz machine list'

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
      - 80
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
