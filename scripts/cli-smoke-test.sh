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

# Count of commands whose exit code did not match expectations, so the run can
# fail loudly instead of always reaching DONE with exit 0.
UNEXPECTED=0

# R [--expect-fail] <ployz args...> : run an operator command on the core and
# log it verbatim. Happy-path commands are expected to exit 0; mark commands
# that should fail (bad ids, missing args, confirm-guarded destructive verbs)
# with --expect-fail. stdin is closed so confirmation prompts get EOF and
# reject instead of blocking on operator input.
R() {
  local expect_fail=0
  if [ "${1:-}" = "--expect-fail" ]; then expect_fail=1; shift; fi
  echo
  echo "═══════════════════════════════════════════════════════════════════"
  echo "\$ ployz $*"
  echo "───────────────────────────────────────────────────────────────────"
  local out rc
  out=$(core "ployz $* 2>&1" </dev/null); rc=$?
  echo "$out"
  echo "[exit $rc]"
  if [ "$expect_fail" = 1 ] && [ "$rc" -eq 0 ]; then
    echo "!! UNEXPECTED SUCCESS (expected a non-zero exit)"; UNEXPECTED=$((UNEXPECTED + 1))
  elif [ "$expect_fail" = 0 ] && [ "$rc" -ne 0 ]; then
    echo "!! UNEXPECTED FAILURE (expected exit 0)"; UNEXPECTED=$((UNEXPECTED + 1))
  fi
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
R --expect-fail --version   # no version flag today

echo; echo "##################### MACHINES #####################"
R machine list
R machine inspect ployz-core
R --expect-fail machine inspect does-not-exist

echo; echo "##################### DEPLOY #####################"
R deploy --image nginx:alpine --route demo.local:80 --replicas 2
R ls
R inspect nginx
R service list
R service inspect nginx
R deploy history
R --expect-fail inspect no-such-service
R --expect-fail deploy   # missing --image

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
R --expect-fail ops status op_bogus_000
R --expect-fail network resolve no.such.host

echo; echo "##################### COMPOSE #####################"
core 'printf "name: demo\nservices:\n  web:\n    image: nginx:alpine\n" > /tmp/valid-compose.yml'
R compose check /tmp/valid-compose.yml
R --expect-fail compose check /tmp/does-not-exist.yml

echo; echo "##################### LIFECYCLE #####################"
R service restart nginx
R deploy --image nginx:alpine --route demo.local:80 --replicas 1
R deploy rollback --last-good
R machine drain ployz-edge
R machine resume ployz-edge

echo; echo "##################### STORAGE / CLEANUP (confirm-guarded) #####################"
R volume list
R --expect-fail volume rm default no-such-volume   # data-loss confirmation prompt
R --expect-fail namespace rm no-such-namespace   # confirmation prompt

echo; echo "##################### HOST-SIDE / AUTH (non-destructive) #####################"
R host --help
R --expect-fail host core-promote --check   # no promotion material on a plain edge-less core
R --expect-fail login   # errors unless Ployz Cloud is configured

echo
if [ "$UNEXPECTED" -eq 0 ]; then
  echo "### CLI SMOKE TEST DONE — all commands exited as expected"
else
  echo "### CLI SMOKE TEST DONE — ${UNEXPECTED} command(s) exited unexpectedly (see !! lines above)"
  exit 1
fi
