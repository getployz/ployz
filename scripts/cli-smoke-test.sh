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

resolve_machine_ids() {
  local machine_list=$1
  local machine_count core_matches edge_matches core_count edge_count

  machine_count=$(printf '%s\n' "$machine_list" | awk 'NF { count++ } END { print count + 0 }')
  if [ "$machine_count" -ne 2 ]; then
    echo "expected exactly two machines, found ${machine_count}" >&2
    return 1
  fi

  core_matches=$(printf '%s\n' "$machine_list" | awk '$2 == "ployz-core" { print $1 }')
  edge_matches=$(printf '%s\n' "$machine_list" | awk '$2 == "ployz-edge" { print $1 }')
  core_count=$(printf '%s\n' "$core_matches" | awk 'NF { count++ } END { print count + 0 }')
  edge_count=$(printf '%s\n' "$edge_matches" | awk 'NF { count++ } END { print count + 0 }')
  if [ "$core_count" -ne 1 ] || [ "$edge_count" -ne 1 ]; then
    echo "expected exactly one ployz-core and one ployz-edge machine" >&2
    return 1
  fi
  if [ -z "$core_matches" ] || [ -z "$edge_matches" ] || [ "$core_matches" = "$edge_matches" ]; then
    echo "resolved machine ids must be non-empty and distinct" >&2
    return 1
  fi

  CORE_MACHINE_ID=$core_matches
  EDGE_MACHINE_ID=$edge_matches
}

has_internal_service_address() {
  local core_machine_id=$1
  local edge_machine_id=$2
  awk -v core_machine_id="$core_machine_id" -v edge_machine_id="$edge_machine_id" '
    function octet(value) {
      return value ~ /^[0-9]+$/ && value + 0 <= 255
    }
    function ipv4(value, parts, count) {
      count = split(value, parts, ".")
      return count == 4 && octet(parts[1]) && octet(parts[2]) \
        && octet(parts[3]) && octet(parts[4])
    }
    NR == 1 && $0 == "answer-sets consistent" { consistent = 1 }
    $1 == "machine" { testimony_rows++ }
    $1 == "machine" && $3 == "nginx.default.internal" && $4 == "A" {
      count = split($5, addresses, ",")
      valid = 0
      for (i = 1; i <= count; i++) {
        if (ipv4(addresses[i])) valid = 1
      }
      if (valid && $2 == core_machine_id) core_answers++
      if (valid && $2 == edge_machine_id) edge_answers++
    }
    END {
      exit !(consistent && testimony_rows == 2 \
        && core_answers == 1 && edge_answers == 1)
    }
  '
}

run_self_test() {
  local valid_list invalid_name_list duplicate_name_list duplicate_id_list
  valid_list=$'machine_core ployz-core control-endpoints ready\nmachine_edge ployz-edge control-endpoints ready'
  invalid_name_list=$'machine_core ployz-core control-endpoints ready\nmachine_other other-edge control-endpoints ready'
  duplicate_name_list=$'machine_core ployz-core control-endpoints ready\nmachine_other ployz-core control-endpoints ready'
  duplicate_id_list=$'machine_same ployz-core control-endpoints ready\nmachine_same ployz-edge control-endpoints ready'

  resolve_machine_ids "$valid_list" || return 1
  [ "$CORE_MACHINE_ID" = machine_core ] || return 1
  [ "$EDGE_MACHINE_ID" = machine_edge ] || return 1
  if resolve_machine_ids "$invalid_name_list" >/dev/null 2>&1 \
    || resolve_machine_ids "$duplicate_name_list" >/dev/null 2>&1 \
    || resolve_machine_ids "$duplicate_id_list" >/dev/null 2>&1; then
    return 1
  fi
  printf '%s\n' \
    'answer-sets consistent' \
    'machine machine_core nginx.default.internal A 10.42.0.8' \
    'machine machine_edge nginx.default.internal A 10.42.0.8' \
    | has_internal_service_address machine_core machine_edge || return 1
  if printf '%s\n' \
      'answer-sets unconfirmed answered=1/2' \
      'machine machine_core nginx.default.internal A 10.42.0.8' \
      'machine machine_edge nginx.default.internal timed out' \
      | has_internal_service_address machine_core machine_edge \
    || printf '%s\n' \
      'answer-sets divergent' \
      'machine machine_core nginx.default.internal A 10.42.0.8' \
      'machine machine_edge nginx.default.internal A 10.42.0.9' \
      | has_internal_service_address machine_core machine_edge \
    || printf '%s\n' \
      'answer-sets consistent' \
      'machine machine_core nginx.default.internal A none' \
      'machine machine_edge nginx.default.internal A none' \
      | has_internal_service_address machine_core machine_edge \
    || printf '%s\n' \
      'answer-sets consistent' \
      'machine machine_core nginx.default.internal A 999.42.0.8' \
      'machine machine_edge nginx.default.internal A 999.42.0.8' \
      | has_internal_service_address machine_core machine_edge; then
    return 1
  fi

  echo "cli smoke regression: PASS"
}

if [ "${1:-}" = --self-test ]; then
  run_self_test
  exit 0
fi

CORE="${1:?usage: cli-smoke-test.sh <core-ip> <edge-ip>}"
EDGE="${2:?usage: cli-smoke-test.sh <core-ip> <edge-ip>}"
SSH_OPTS=(-o StrictHostKeyChecking=accept-new -o GSSAPIAuthentication=no \
          -o ConnectTimeout=20 -o ServerAliveInterval=15)
core() { ssh "${SSH_OPTS[@]}" "root@${CORE}" "$@"; }

# Count of commands whose exit code did not match expectations, so the run can
# fail loudly instead of always reaching DONE with exit 0.
UNEXPECTED=0
LAST_OUTPUT=
LAST_EXIT=0

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
  LAST_OUTPUT=$out
  LAST_EXIT=$rc
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
  core "ployz machine init root@${CORE} --name ployz-core --public-ip ${CORE}" >/dev/null 2>&1
  core_key=$(core 'cat ~/.ssh/id_ed25519.pub')
  ssh "${SSH_OPTS[@]}" "root@${EDGE}" "grep -qF '${core_key}' ~/.ssh/authorized_keys 2>/dev/null || echo '${core_key}' >> ~/.ssh/authorized_keys"
  core "ssh-keyscan -T 8 ${EDGE} >> ~/.ssh/known_hosts 2>/dev/null"
  core "ployz machine add root@${EDGE} --name ployz-edge" >/dev/null 2>&1
fi
echo "### version: $(core 'grep -h PLOYZ_VERSION /etc/ployz/release.env')"

machine_list_output=$(core 'ployz machine list 2>&1')
machine_list_exit=$?
if [ "$machine_list_exit" -ne 0 ]; then
  printf '%s\n' "$machine_list_output" >&2
  echo "machine list failed with exit ${machine_list_exit}" >&2
  exit 1
fi
printf '%s\n' "$machine_list_output"
resolve_machine_ids "$machine_list_output" || exit 1

echo; echo "##################### TOP-LEVEL #####################"
R --help
R --expect-fail --version   # no version flag today

echo; echo "##################### MACHINES #####################"
R machine list
R machine inspect "$CORE_MACHINE_ID"
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
R network resolve nginx.default.internal
if [ "$LAST_EXIT" -eq 0 ] \
  && ! printf '%s\n' "$LAST_OUTPUT" \
    | has_internal_service_address "$CORE_MACHINE_ID" "$EDGE_MACHINE_ID"; then
  echo "!! INTERNAL DNS DID NOT RETURN AN IPV4 ADDRESS"
  UNEXPECTED=$((UNEXPECTED + 1))
fi
R logs nginx --tail 10
CID=$(core 'ployz inspect nginx 2>&1' | grep -oE 'container [0-9a-f]{64}' | head -1 | awk '{print $2}')
[ -n "$CID" ] && R logs tail "$CID" --machine "$CORE_MACHINE_ID" --tail 5
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
R machine drain "$EDGE_MACHINE_ID"
R machine resume "$EDGE_MACHINE_ID"

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
