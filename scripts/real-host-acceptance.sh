#!/usr/bin/env bash
# Two-machine real-host acceptance for ployz.
#
#   scripts/real-host-acceptance.sh <core-ip> <edge-ip>
#
# Forms a fresh cluster on the two hosts you pass, deploys an image-based
# service across both machines, then checks cross-machine routing and
# daemon-restart invisibility. Prints per-step timing.
#
# Requirements:
#   - Two FRESH Ubuntu 24.04+ amd64 hosts, kernel 6.6+ (tcx eBPF). 1 GB RAM is
#     enough. The hosts install the public alpha channel from https://ployz.sh.
#   - Run from a machine with root SSH to BOTH hosts (key in your ssh-agent).
#     If your local uplink to the hosts is flaky, run this from a small VM that
#     has stable SSH to them instead.
#
# See docs/operations/real-host-acceptance.md. Tear the hosts down when done.
set -u

CORE="${1:?usage: real-host-acceptance.sh <core-ip> <edge-ip>}"
EDGE="${2:?usage: real-host-acceptance.sh <core-ip> <edge-ip>}"

# accept-new + GSSAPI off avoids the fresh-cloud-host auth stall; the operator
# commands run over SSH on the core, so we forward nothing sensitive here.
SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o GSSAPIAuthentication=no \
          -o ConnectTimeout=20 -o ServerAliveInterval=15)
ts() { date +%s; }
log() { echo "[$(date +%H:%M:%S)] $*"; }
core() { ssh "${SSH_OPTS[@]}" "root@${CORE}" "$@"; }

log "waiting for both hosts to accept SSH"
for host in "$CORE" "$EDGE"; do
  for _ in $(seq 1 50); do
    ssh "${SSH_OPTS[@]}" "root@${host}" true 2>/dev/null && break || sleep 6
  done
done

# The operator shells out to `ssh` for init/add; disable GSSAPI there too.
for host in "$CORE" "$EDGE"; do
  ssh "${SSH_OPTS[@]}" "root@${host}" \
    'mkdir -p ~/.ssh; printf "Host *\n  GSSAPIAuthentication no\n  StrictHostKeyChecking accept-new\n" > ~/.ssh/config; chmod 600 ~/.ssh/config'
done

# Core is its own operator target: init dials the core over SSH, so it needs a
# key that authenticates to itself, and the core's public IP in known_hosts.
core '[ -f ~/.ssh/id_ed25519 ] || ssh-keygen -t ed25519 -N "" -f ~/.ssh/id_ed25519 -q; grep -qF "$(cat ~/.ssh/id_ed25519.pub)" ~/.ssh/authorized_keys || cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys'
core "ssh-keyscan -T 8 ${CORE} 127.0.0.1 >> ~/.ssh/known_hosts 2>/dev/null"

log "installing the ployz CLI on the core"
core 'command -v ployz >/dev/null 2>&1 || { curl -fsSL https://ployz.sh -o /tmp/ployz.sh && sh /tmp/ployz.sh >/dev/null 2>&1; }'
log "version: $(core 'grep -h PLOYZ_VERSION /etc/ployz/release.env')"

# Init targets the core's PUBLIC IP: it is in the machine cert SAN and, unlike
# 127.0.0.1, is the address edges are told to dial for NATS.
log "machine init (first machine)"
t0=$(ts)
core "ployz machine init root@${CORE} --public-ip ${CORE} --public-url none" 2>&1 | tail -4
log "TIMING machine-init=$(( $(ts)-t0 ))s"

# Authorize the core's key on the edge so `machine add` can dial it.
core_key=$(core 'cat ~/.ssh/id_ed25519.pub')
ssh "${SSH_OPTS[@]}" "root@${EDGE}" \
  "grep -qF '${core_key}' ~/.ssh/authorized_keys 2>/dev/null || echo '${core_key}' >> ~/.ssh/authorized_keys"
core "ssh-keyscan -T 8 ${EDGE} >> ~/.ssh/known_hosts 2>/dev/null"

log "machine add (edge join)"
t0=$(ts)
core "ployz machine add root@${EDGE}" 2>&1 | tail -6
log "TIMING machine-add=$(( $(ts)-t0 ))s"

log "machine list:"
core 'ployz machine list' 2>&1

log "deploy nginx (2 replicas, route demo.local:80)"
t0=$(ts)
core "ployz deploy --image nginx:alpine --route demo.local:80 --replicas 2" 2>&1 | tail -8
log "TIMING deploy=$(( $(ts)-t0 ))s"

log "cross-machine routing (Host: demo.local via each gateway)"
pass=1
for host in "$CORE" "$EDGE"; do
  code=$(ssh "${SSH_OPTS[@]}" "root@${host}" \
    "curl -s -o /dev/null -w '%{http_code}' -H 'Host: demo.local' http://127.0.0.1/ 2>/dev/null")
  log "  gateway ${host} -> HTTP ${code}"
  [ "$code" = "200" ] || pass=0
done

log "restart invisibility: restart ployzd-control on the core, re-check route"
if core 'systemctl restart ployzd-control'; then
  core 'sleep 3'
  active=$(core 'systemctl is-active ployzd-control 2>/dev/null')
  log "  ployzd-control after restart: ${active}"
  [ "$active" = "active" ] || { log "  ployzd-control did not come back active"; pass=0; }
else
  log "  ployzd-control restart FAILED"; pass=0
fi
code=$(core "curl -s -o /dev/null -w '%{http_code}' -H 'Host: demo.local' http://127.0.0.1/ 2>/dev/null")
log "  post-restart route -> HTTP ${code}"
[ "$code" = "200" ] || pass=0

if [ "$pass" = "1" ]; then
  log "ACCEPTANCE PASSED"
else
  log "ACCEPTANCE FAILED (a route check did not return 200)"
  exit 1
fi
