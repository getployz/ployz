#!/bin/sh
set -eu

command="${1:-}"

usage() {
  cat >&2 <<'USAGE'
usage:
  HCLOUD_TOKEN=... HETZNER_SSH_KEY=... PLOYZ_SSH_PRIVATE_KEY=... scripts/hetzner-two-node-acceptance.sh up --run-id <id>
  HCLOUD_TOKEN=... scripts/hetzner-two-node-acceptance.sh cleanup --run-id <id>

optional env:
  HETZNER_LOCATION=fsn1
  HETZNER_SERVER_TYPE=cx22
  HETZNER_IMAGE=ubuntu-24.04
  PLOYZ_SSH_USER=root
  PLOYZ_SSH_READY_TIMEOUT_SECONDS=300
  PLOYZ_ACCEPTANCE_KEEP=1
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

need_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

parse_run_id() {
  run_id=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --run-id)
        [ "$#" -ge 2 ] || die "--run-id needs a value"
        run_id="$2"
        shift 2
        ;;
      *)
        die "unknown argument: $1"
        ;;
    esac
  done

  [ -n "$run_id" ] || die "set --run-id <id>"
  case "$run_id" in
    *[!a-z0-9-]*)
      die "run id must contain only lowercase letters, digits, and hyphens"
      ;;
  esac
}

selector() {
  printf 'ployz=acceptance,ployz_run=%s' "$run_id"
}

server_name() {
  role="$1"
  printf 'ployz-%s-%s' "$run_id" "$role"
}

cleanup_command() {
  printf 'HCLOUD_TOKEN=... scripts/hetzner-two-node-acceptance.sh cleanup --run-id %s\n' "$run_id"
}

server_ip() {
  name="$1"
  hcloud server describe "$name" -o json | jq -r '.public_net.ipv4.ip'
}

create_server() {
  name="$1"
  echo "creating ${name}" >&2
  hcloud server create \
    --name "$name" \
    --type "$server_type" \
    --image "$image" \
    --location "$location" \
    --ssh-key "$HETZNER_SSH_KEY" \
    --label ployz=acceptance \
    --label "ployz_run=${run_id}" \
    --label ployz_cleanup=true \
    --without-ipv6 >/dev/null
  server_ip "$name"
}

cleanup_servers() {
  names="$(hcloud server list --selector "$(selector)" -o noheader -o columns=name)"
  if [ -z "$names" ]; then
    echo "cleanup: no servers found for $(selector)" >&2
    return 0
  fi

  echo "$names" | while IFS= read -r name; do
    [ -n "$name" ] || continue
    echo "cleanup: deleting ${name}" >&2
    hcloud server delete "$name" >/dev/null
  done
}

wait_for_ssh() {
  ip="$1"
  deadline=$(( $(date +%s) + ssh_ready_timeout_seconds ))

  while [ "$(date +%s)" -lt "$deadline" ]; do
    if ssh \
      -i "$PLOYZ_SSH_PRIVATE_KEY" \
      -o BatchMode=yes \
      -o ConnectTimeout=5 \
      -o StrictHostKeyChecking=accept-new \
      -o UserKnownHostsFile="$known_hosts_file" \
      "${ssh_user}@${ip}" true >/dev/null 2>&1; then
      echo "ssh ready: ${ssh_user}@${ip}" >&2
      return 0
    fi
    sleep 5
  done

  return 1
}

on_exit() {
  status="$?"
  if [ -n "${known_hosts_file:-}" ]; then
    rm -f "$known_hosts_file"
  fi
  if [ "$status" -ne 0 ] && [ "${command:-}" = "up" ]; then
    echo "substrate smoke failed; attempting cleanup for run ${run_id}" >&2
    if ! cleanup_servers; then
      echo "automatic cleanup failed; run:" >&2
      cleanup_command >&2
    fi
  fi
  exit "$status"
}

[ -n "$command" ] || {
  usage
  exit 1
}
shift

location="${HETZNER_LOCATION:-fsn1}"
server_type="${HETZNER_SERVER_TYPE:-cx22}"
image="${HETZNER_IMAGE:-ubuntu-24.04}"
ssh_user="${PLOYZ_SSH_USER:-root}"
ssh_ready_timeout_seconds="${PLOYZ_SSH_READY_TIMEOUT_SECONDS:-300}"

case "$command" in
  up)
    parse_run_id "$@"
    need_command hcloud
    need_command jq
    need_command ssh
    [ -n "${HCLOUD_TOKEN:-}" ] || die "set HCLOUD_TOKEN"
    [ -n "${HETZNER_SSH_KEY:-}" ] || die "set HETZNER_SSH_KEY"
    [ -n "${PLOYZ_SSH_PRIVATE_KEY:-}" ] || die "set PLOYZ_SSH_PRIVATE_KEY"
    [ -f "$PLOYZ_SSH_PRIVATE_KEY" ] || die "PLOYZ_SSH_PRIVATE_KEY does not exist: ${PLOYZ_SSH_PRIVATE_KEY}"

    known_hosts_file="$(mktemp)"
    trap on_exit EXIT

    core_name="$(server_name core-1)"
    edge_name="$(server_name edge-2)"
    core_ip="$(create_server "$core_name")"
    edge_ip="$(create_server "$edge_name")"

    wait_for_ssh "$core_ip" || die "ssh readiness failed for ${core_name} (${core_ip})"
    wait_for_ssh "$edge_ip" || die "ssh readiness failed for ${edge_name} (${edge_ip})"

    echo "substrate ready:"
    echo "  ${core_name} ${core_ip}"
    echo "  ${edge_name} ${edge_ip}"

    if [ "${PLOYZ_ACCEPTANCE_KEEP:-0}" != "1" ]; then
      cleanup_servers
      echo "substrate cleanup complete"
    else
      echo "cleanup command: $(cleanup_command)"
    fi
    ;;
  cleanup)
    parse_run_id "$@"
    need_command hcloud
    [ -n "${HCLOUD_TOKEN:-}" ] || die "set HCLOUD_TOKEN"
    cleanup_servers
    ;;
  *)
    usage
    exit 1
    ;;
esac
