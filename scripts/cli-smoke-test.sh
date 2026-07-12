#!/usr/bin/env bash
# Smoke-test every realistic ployz CLI command against a live two-machine
# cluster and print each command with its real output and exit code.
#
#   scripts/cli-smoke-test.sh <core-ip> <edge-ip>
#
# Forms the cluster if it is not already formed (so it runs standalone on fresh
# hosts, or against a cluster left by real-host-acceptance.sh). Happy and
# unhappy paths. Destructive verbs (core promote/demote, host bootstrap,
# machine update, namespace/volume rm on real data) are intentionally exercised
# only in their no-op / confirmation-guarded forms.
#
# Same host requirements as real-host-acceptance.sh. See
# docs/operations/real-host-acceptance.md. Tear the hosts down when done.
set -u

CORE="${1:?usage: cli-smoke-test.sh <core-ip> <edge-ip>}"
EDGE="${2:?usage: cli-smoke-test.sh <core-ip> <edge-ip>}"
SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o GSSAPIAuthentication=no \
          -o ConnectTimeout=20 -o ServerAliveInterval=15)
core() { ssh "${SSH_OPTS[@]}" "root@${CORE}" "$@"; }

# R <ployz args...> : run an operator command on the core and log it verbatim.
R() {
  echo
  echo "═══════════════════════════════════════════════════════════════════"
  echo "\$ ployz $*"
  echo "───────────────────────────────────────────────────────────────────"
  local out rc
  out=$(core "ployz $* 2>&1"); rc=$?
  echo "$out"
  echo "[exit $rc]"
}

# --- form the cluster only if it is not already up -------------------------
for host in "$CORE" "$EDGE"; do
  for _ in $(seq 1 50); do ssh "${SSH_OPTS[@]}" "root@${host}" true 2>/dev/null && break || sleep 6; done
  ssh "${SSH_OPTS[@]}" "root@${host}" 'mkdir -p ~/.ssh; printf "Host *\n  GSSAPIAuthentication no\n  StrictHostKeyChecking accept-new\n" > ~/.ssh/config; chmod 600 ~/.ssh/config'
done
core 'command -v ployz >/dev/null 2>&1 || { curl -fsSL https://ployz.sh -o /tmp/ployz.sh && sh /tmp/ployz.sh >/dev/null 2>&1; }'

if [ "$(core 'ployz machine list 2>/dev/null | wc -l')" -ge 2 ]; then
  echo "### cluster already formed — running against it"
else
  echo "### forming a fresh cluster"
  core '[ -f ~/.ssh/id_ed25519 ] || ssh-keygen -t ed25519 -N "" -f ~/.ssh/id_ed25519 -q; grep -qF "$(cat ~/.ssh/id_ed25519.pub)" ~/.ssh/authorized_keys || cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys'
  core "ssh-keyscan -T 8 ${CORE} 127.0.0.1 >> ~/.ssh/known_hosts 2>/dev/null"
  core "ployz machine init root@${CORE} --public-ip ${CORE} --public-url none" >/dev/null 2>&1
  core_key=$(core 'cat ~/.ssh/id_ed25519.pub')
  ssh "${SSH_OPTS[@]}" "root@${EDGE}" "grep -qF '${core_key}' ~/.ssh/authorized_keys 2>/dev/null || echo '${core_key}' >> ~/.ssh/authorized_keys"
  core "ssh-keyscan -T 8 ${EDGE} >> ~/.ssh/known_hosts 2>/dev/null"
  core "ployz machine add root@${EDGE}" >/dev/null 2>&1
fi
echo "### version: $(core 'grep -h PLOYZ_VERSION /etc/ployz/release.env')"

echo; echo "##################### TOP-LEVEL #####################"
R --help
R --version   # unhappy: no version flag today

echo; echo "##################### MACHINES #####################"
R machine list
R machine inspect ployz-core
R machine inspect does-not-exist   # unhappy

echo; echo "##################### DEPLOY #####################"
R deploy --image nginx:alpine --route demo.local:80 --replicas 2
R ls
R inspect nginx
R service list
R service inspect nginx
R deploy history
R inspect no-such-service          # unhappy
R deploy                           # unhappy: missing --image

echo; echo "##################### OBSERVE #####################"
R ops list
R ops list --active
OP=$(core 'ployz ops list 2>&1' | grep -oE 'op_deploy_[A-Za-z0-9]+' | head -1)
[ -n "$OP" ] && R ops status "$OP"
[ -n "$OP" ] && R ops watch "$OP" --json
R network status
R network status --probe
R network resolve demo.local
R logs nginx --tail 10
CID=$(core 'ployz inspect nginx 2>&1' | grep -oE 'container [0-9a-f]{64}' | head -1 | awk '{print $2}')
[ -n "$CID" ] && R logs tail "$CID" --machine ployz-core --tail 5
R ops status op_bogus_000          # unhappy
R network resolve no.such.host     # unhappy

echo; echo "##################### COMPOSE #####################"
core 'printf "name: demo\nservices:\n  web:\n    image: nginx:alpine\n" > /tmp/valid-compose.yml'
R compose check /tmp/valid-compose.yml
R compose check /tmp/does-not-exist.yml   # unhappy

echo; echo "##################### LIFECYCLE #####################"
R service restart nginx
R deploy --image nginx:alpine --route demo.local:80 --replicas 1
R deploy rollback --last-good
R machine drain ployz-edge
R machine resume ployz-edge

echo; echo "##################### STORAGE / CLEANUP (confirm-guarded) #####################"
R volume list
R volume rm default no-such-volume     # unhappy: prints data-loss confirmation prompt
R namespace rm no-such-namespace       # unhappy: prints confirmation prompt

echo; echo "##################### HOST-SIDE / AUTH (non-destructive) #####################"
R host --help
R host core-promote --check
R login

echo; echo "### CLI SMOKE TEST DONE"
