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
  PLOYZ_ACCEPTANCE_TARGET_DIR=/tmp/ployz-build-target
  PLOYZ_ACCEPTANCE_PLOYZCTL=/tmp/ployz-build-target/debug/ployzctl
  PLOYZ_ACCEPTANCE_PLOYZD=/tmp/ployz-build-target/debug/ployzd
  PLOYZ_ACCEPTANCE_KEEPER=/tmp/ployz-build-target/debug/ployz-keeper
  PLOYZ_ACCEPTANCE_NATS_SERVER=/path/to/nats-server
  PLOYZ_ACCEPTANCE_EBPF_BYTECODE=/tmp/ployz-rust-ebpf-target/bpfel-unknown-none/release/ployz-ebpf-tc
  PLOYZ_ACCEPTANCE_PLOYZ_SH=scripts/ployz.sh
  PLOYZ_ACCEPTANCE_SMOKE_IMAGE=nginx:alpine
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

need_file() {
  path="$1"
  label="$2"
  [ -f "$path" ] || die "${label} does not exist: ${path}"
}

sha256_file() {
  path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
    return 0
  fi
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
    return 0
  fi
  die "missing required command: sha256sum or shasum"
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

ssh_target() {
  ip="$1"
  printf '%s@%s' "$ssh_user" "$ip"
}

ssh_base() {
  ssh \
    -i "$PLOYZ_SSH_PRIVATE_KEY" \
    -o BatchMode=yes \
    -o ConnectTimeout=10 \
    -o StrictHostKeyChecking=accept-new \
    -o UserKnownHostsFile="$known_hosts_file" \
    "$@"
}

scp_base() {
  scp \
    -i "$PLOYZ_SSH_PRIVATE_KEY" \
    -o BatchMode=yes \
    -o ConnectTimeout=10 \
    -o StrictHostKeyChecking=accept-new \
    -o UserKnownHostsFile="$known_hosts_file" \
    "$@"
}

remote_sh() {
  ip="$1"
  shift
  ssh_base "$(ssh_target "$ip")" "$@"
}

prepare_host_runtime() {
  ip="$1"
  remote_sh "$ip" "if ! command -v docker >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1 || ! command -v wg >/dev/null 2>&1 || ! command -v tc >/dev/null 2>&1; then apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl docker.io wireguard-tools iproute2; fi"
  remote_sh "$ip" "systemctl enable --now docker"
  remote_sh "$ip" "docker info >/dev/null"
  remote_sh "$ip" "mountpoint -q /sys/fs/bpf || mount -t bpf bpf /sys/fs/bpf"
}

stage_host() {
  ip="$1"
  remote_sh "$ip" "install -d -m 0755 '$remote_dir'"
  remote_sh "$ip" "install -d -m 0755 /usr/local/lib/ployz/ebpf"
  scp_base "$ployzctl_bin" "$(ssh_target "$ip"):$remote_ployzctl" >/dev/null
  scp_base "$ployzd_bin" "$(ssh_target "$ip"):$remote_ployzd" >/dev/null
  scp_base "$keeper_bin" "$(ssh_target "$ip"):$remote_keeper" >/dev/null
  scp_base "$nats_server_bin" "$(ssh_target "$ip"):$remote_nats_server" >/dev/null
  scp_base "$ployz_sh" "$(ssh_target "$ip"):$remote_ployz_sh" >/dev/null
  scp_base "$ebpf_ctl_bin" "$(ssh_target "$ip"):$remote_ebpf_ctl_source" >/dev/null
  scp_base "$ebpf_bytecode" "$(ssh_target "$ip"):$remote_ebpf_bytecode_source" >/dev/null
  remote_sh "$ip" "chmod 0755 '$remote_ployzctl' '$remote_ployzd' '$remote_keeper' '$remote_nats_server' '$remote_ployz_sh' '$remote_ebpf_ctl_source'"
  remote_sh "$ip" "chmod 0644 '$remote_ebpf_bytecode_source'"
}

run_remote_logged() {
  label="$1"
  ip="$2"
  shift 2
  log_file="${log_dir}/${label}.log"
  echo "running ${label}" >&2
  if remote_sh "$ip" "$@" >"$log_file" 2>&1; then
    cat "$log_file"
    return 0
  fi
  echo "command failed: ${label}" >&2
  echo "output: ${log_file}" >&2
  echo "cleanup command: $(cleanup_command)" >&2
  cat "$log_file" >&2
  return 1
}

extract_join_token() {
  awk '$1 == "join-token" { print $2 }' "$1" | tail -n 1
}

extract_bootstrap_tunnel_command() {
  awk '$1 == "bootstrap-tunnel" { sub(/^bootstrap-tunnel /, ""); print }' "$1" | tail -n 1
}

wait_for_machine_ready() {
  ip="$1"
  node="$2"
  expected_public_ip="$3"
  deadline=$(( $(date +%s) + 120 ))
  log_file="${log_dir}/machine-${node}-ready.log"

  while [ "$(date +%s)" -lt "$deadline" ]; do
    if remote_sh "$ip" "PLOYZ_NATS_URL=nats://127.0.0.1:4222 '$remote_ployzctl' machine inspect '$node'" >"$log_file" 2>&1; then
      if grep -q "public-ip ${expected_public_ip}" "$log_file" && grep -q "gateway current" "$log_file"; then
        cat "$log_file"
        return 0
      fi
    fi
    sleep 2
  done

  echo "machine ${node} did not become ready; last output:" >&2
  cat "$log_file" >&2 || true
  return 1
}

wait_for_smoke_service() {
  name="$1"
  ip="$2"
  deadline=$(( $(date +%s) + 120 ))
  log_file="${log_dir}/curl-smoke-${name}.log"

  while [ "$(date +%s)" -lt "$deadline" ]; do
    if curl -fsS --connect-timeout 2 --max-time 5 -H 'Host: smoke.local' "http://${ip}:8080/" >"$log_file" 2>&1; then
      cat "$log_file"
      return 0
    fi
    sleep 2
  done

  echo "smoke service did not respond through ${name} gateway ${ip}; last output:" >&2
  cat "$log_file" >&2 || true
  return 1
}

wait_for_deploy_operation() {
  ip="$1"
  operation_id="$2"
  deadline=$(( $(date +%s) + 180 ))
  log_file="${log_dir}/watch-deploy.log"

  while [ "$(date +%s)" -lt "$deadline" ]; do
    if remote_sh "$ip" "PLOYZ_NATS_URL=nats://127.0.0.1:4222 '$remote_ployzctl' ops watch '$operation_id'" >"$log_file" 2>&1; then
      if grep -q "deploy.wireguard_ebpf_prepared" "$log_file" && grep -q "deploy.completed" "$log_file"; then
        cat "$log_file"
        return 0
      fi
      if grep -q "deploy.failed" "$log_file"; then
        echo "deploy operation ${operation_id} failed; output:" >&2
        cat "$log_file" >&2
        return 1
      fi
    fi
    sleep 2
  done

  echo "deploy operation ${operation_id} did not reach dataplane-ready completion; last output:" >&2
  cat "$log_file" >&2 || true
  return 1
}

wait_for_machine_add_operation() {
  ip="$1"
  operation_id="$2"
  deadline=$(( $(date +%s) + 120 ))
  log_file="${log_dir}/watch-machine-add.log"

  while [ "$(date +%s)" -lt "$deadline" ]; do
    if remote_sh "$ip" "PLOYZ_NATS_URL=nats://127.0.0.1:4222 '$remote_ployzctl' ops watch '$operation_id'" >"$log_file" 2>&1; then
      if grep -q "machine.add.completed" "$log_file"; then
        cat "$log_file"
        return 0
      fi
      if grep -q "machine.add.failed" "$log_file"; then
        echo "machine add operation ${operation_id} failed; output:" >&2
        cat "$log_file" >&2
        return 1
      fi
    fi
    sleep 2
  done

  echo "machine add operation ${operation_id} did not complete; last output:" >&2
  cat "$log_file" >&2 || true
  return 1
}

on_exit() {
  status="$?"
  if [ -n "${known_hosts_file:-}" ]; then
    rm -f "$known_hosts_file"
  fi
  if [ "$status" -ne 0 ] && [ "${command:-}" = "up" ]; then
    echo "two-node proof failed; attempting cleanup for run ${run_id}" >&2
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
acceptance_target_dir="${PLOYZ_ACCEPTANCE_TARGET_DIR:-${CARGO_TARGET_DIR:-/tmp/ployz-build-target}}"
ployzctl_bin="${PLOYZ_ACCEPTANCE_PLOYZCTL:-${acceptance_target_dir}/debug/ployzctl}"
ployzd_bin="${PLOYZ_ACCEPTANCE_PLOYZD:-${acceptance_target_dir}/debug/ployzd}"
keeper_bin="${PLOYZ_ACCEPTANCE_KEEPER:-${acceptance_target_dir}/debug/ployz-keeper}"
nats_server_bin="${PLOYZ_ACCEPTANCE_NATS_SERVER:-}"
ebpf_ctl_bin="${PLOYZ_ACCEPTANCE_EBPF_CTL:-${acceptance_target_dir}/debug/ployz-ebpf-ctl}"
ebpf_bytecode="${PLOYZ_ACCEPTANCE_EBPF_BYTECODE:-/tmp/ployz-rust-ebpf-target/bpfel-unknown-none/release/ployz-ebpf-tc}"
smoke_image="${PLOYZ_ACCEPTANCE_SMOKE_IMAGE:-nginx:alpine}"
ployz_sh="${PLOYZ_ACCEPTANCE_PLOYZ_SH:-scripts/ployz.sh}"
edge_runtime_nats_addr="127.0.0.1:7422"
edge_runtime_nats_url="nats://${edge_runtime_nats_addr}"
remote_ebpf_path="/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
remote_ebpf_ctl="/usr/local/bin/ployz-ebpf-ctl"

case "$command" in
  up)
    parse_run_id "$@"
    [ -n "${HCLOUD_TOKEN:-}" ] || die "set HCLOUD_TOKEN"
    [ -n "${HETZNER_SSH_KEY:-}" ] || die "set HETZNER_SSH_KEY"
    [ -n "${PLOYZ_SSH_PRIVATE_KEY:-}" ] || die "set PLOYZ_SSH_PRIVATE_KEY"
    [ -f "$PLOYZ_SSH_PRIVATE_KEY" ] || die "PLOYZ_SSH_PRIVATE_KEY does not exist: ${PLOYZ_SSH_PRIVATE_KEY}"
    [ -n "$nats_server_bin" ] || die "set PLOYZ_ACCEPTANCE_NATS_SERVER"
    need_file "$ployzctl_bin" "Ployz CLI"
    need_file "$ployzd_bin" "ployzd artifact"
    need_file "$keeper_bin" "ployz-keeper artifact"
    need_file "$nats_server_bin" "nats-server artifact"
    need_file "$ebpf_ctl_bin" "Ployz eBPF ctl artifact"
    need_file "$ebpf_bytecode" "Ployz eBPF bytecode"
    need_file "$ployz_sh" "ployz.sh"
    need_command hcloud
    need_command jq
    need_command ssh
    need_command scp
    need_command awk
    need_command curl

    known_hosts_file="$(mktemp)"
    log_dir="${TMPDIR:-/tmp}/ployz-acceptance-${run_id}-logs"
    rm -rf "$log_dir"
    mkdir -p "$log_dir"
    trap on_exit EXIT
    remote_dir="/tmp/ployz-acceptance-${run_id}"
    remote_ployzctl="${remote_dir}/ployzctl"
    remote_ployzd="${remote_dir}/ployzd"
    remote_keeper="${remote_dir}/ployz-keeper"
    remote_nats_server="${remote_dir}/nats-server"
    remote_ployz_sh="${remote_dir}/ployz.sh"
    remote_ebpf_ctl_source="${remote_dir}/ployz-ebpf-ctl"
    remote_ebpf_bytecode_source="${remote_dir}/ployz-ebpf-tc"
    remote_bootstrap_tunnel_pid="${remote_dir}/bootstrap-tunnel.pid"
    remote_bootstrap_tunnel_log="${remote_dir}/bootstrap-tunnel.log"
    remote_bootstrap_tunnel_secret="${remote_dir}/bootstrap-tunnel.key"
    remote_bootstrap_tunnel_public="${remote_dir}/bootstrap-tunnel.public"
    remote_join_template="/etc/ployz/machine-join-template.json"
    remote_secret_delivery="/etc/ployz/machine-join-secret-delivery.json"
    remote_core_iroh_secret="/var/lib/ployz/iroh/endpoint.key"
    core_iroh_port="4433"
    ployzd_sha256="$(sha256_file "$ployzd_bin")"
    keeper_sha256="$(sha256_file "$keeper_bin")"
    nats_sha256="$(sha256_file "$nats_server_bin")"
    ebpf_ctl_sha256="$(sha256_file "$ebpf_ctl_bin")"
    ebpf_bytecode_sha256="$(sha256_file "$ebpf_bytecode")"

    core_name="$(server_name core-1)"
    edge_name="$(server_name edge-2)"
    core_ip="$(create_server "$core_name")"
    edge_ip="$(create_server "$edge_name")"

    wait_for_ssh "$core_ip" || die "ssh readiness failed for ${core_name} (${core_ip})"
    wait_for_ssh "$edge_ip" || die "ssh readiness failed for ${edge_name} (${edge_ip})"

    echo "disposable hosts ready:"
    echo "  ${core_name} ${core_ip}"
    echo "  ${edge_name} ${edge_ip}"

    prepare_host_runtime "$core_ip"
    prepare_host_runtime "$edge_ip"
    stage_host "$core_ip"
    stage_host "$edge_ip"
    remote_sh "$core_ip" "install -d -m 0700 /etc/ployz"
    remote_sh "$core_ip" "umask 077; cat > '$remote_secret_delivery'" <<'JSON'
{"nats_credentials":"acceptance-node-creds","core_iroh_ticket":"acceptance-core-ticket"}
JSON
    core_iroh_identity_log="${log_dir}/core-iroh-identity.log"
    run_remote_logged core-iroh-identity "$core_ip" \
      "'$remote_ployzd' tunnel identity --secret-key-file '$remote_core_iroh_secret'" >"$core_iroh_identity_log"
    core_iroh_public_key="$(awk '$1 == "public-key" { print $2 }' "$core_iroh_identity_log" | tail -n 1)"
    [ -n "$core_iroh_public_key" ] || die "core iroh identity did not print a public key; output: ${core_iroh_identity_log}"

    run_remote_logged render-join-template "$core_ip" \
      "'$remote_ployzctl' init join-template --cluster 'acceptance-${run_id}' --runtime-nats-url '$edge_runtime_nats_url' --trusted-first-node core_1 --core-iroh-public-key '$core_iroh_public_key' --core-iroh-direct-address '${core_ip}:${core_iroh_port}' --ployzd-version acceptance --ployzd-source '$remote_ployzd' --ployzd-sha256 '$ployzd_sha256' --ployzd-install-path /usr/local/bin/ployzd --ebpf-bytecode-version acceptance --ebpf-bytecode-source '$remote_ebpf_bytecode_source' --ebpf-bytecode-sha256 '$ebpf_bytecode_sha256' --ebpf-bytecode-install-path '$remote_ebpf_path' --ebpf-ctl-version acceptance --ebpf-ctl-source '$remote_ebpf_ctl_source' --ebpf-ctl-sha256 '$ebpf_ctl_sha256' --ebpf-ctl-install-path '$remote_ebpf_ctl' --secret-delivery-file '$remote_secret_delivery' > '$remote_join_template'"

    run_remote_logged install-core "$core_ip" \
      "'$remote_keeper' first-node-install --node core_1 --node-public-ip '$core_ip' --gateway --ployzd-version acceptance --ployzd-source '$remote_ployzd' --ployzd-sha256 '$ployzd_sha256' --ployzd-install-path /usr/local/bin/ployzd --ebpf-bytecode-version acceptance --ebpf-bytecode-source '$remote_ebpf_bytecode_source' --ebpf-bytecode-sha256 '$ebpf_bytecode_sha256' --ebpf-bytecode-install-path '$remote_ebpf_path' --ebpf-ctl-version acceptance --ebpf-ctl-source '$remote_ebpf_ctl_source' --ebpf-ctl-sha256 '$ebpf_ctl_sha256' --ebpf-ctl-install-path '$remote_ebpf_ctl' --nats-version acceptance --nats-source '$remote_nats_server' --nats-sha256 '$nats_sha256' --nats-binary /usr/local/bin/nats-server --nats-config /etc/nats/nats-server.conf --machine-bootstrap-url https://get.ployz.dev/ployz.sh --machine-join-template-file '$remote_join_template'"

    run_remote_logged activate-core "$core_ip" \
      "PLOYZ_NATS_URL=nats://127.0.0.1:4222 '$remote_ployzctl' init activate-first-node --node core_1 --gateway"

    machine_log="${log_dir}/machine-add.log"
    run_remote_logged machine-add "$core_ip" \
      "PLOYZ_NATS_URL=nats://127.0.0.1:4222 '$remote_ployzctl' machine add --node edge_2 --name edge_2 --operation op_machine_add --idempotency-key idem_machine_add --gateway"
    join_token="$(extract_join_token "$machine_log")"
    [ -n "$join_token" ] || die "machine add did not print a join token; output: ${machine_log}"
    bootstrap_tunnel_command="$(extract_bootstrap_tunnel_command "$machine_log")"
    [ -n "$bootstrap_tunnel_command" ] || die "machine add did not print a bootstrap tunnel command; output: ${machine_log}"
    grep -q "PLOYZ_NATS_URL='$edge_runtime_nats_url'" "$machine_log" || die "machine add did not print the joined node runtime NATS URL; output: ${machine_log}"

    run_remote_logged start-bootstrap-tunnel "$edge_ip" \
      "PLOYZ_TUNNEL_SECRET_KEY_FILE='$remote_bootstrap_tunnel_secret' PLOYZ_TUNNEL_PUBLIC_KEY_FILE='$remote_bootstrap_tunnel_public' PLOYZ_TUNNEL_IROH_BIND_ADDR=0.0.0.0:0 nohup ${bootstrap_tunnel_command%ployzd tunnel --side edge}'$remote_ployzd' tunnel --side edge > '$remote_bootstrap_tunnel_log' 2>&1 & echo \$! > '$remote_bootstrap_tunnel_pid'"

    run_remote_logged join-edge "$edge_ip" \
      "PLOYZ_KEEPER_URL='file://${remote_keeper}' PLOYZ_KEEPER_SHA256='$keeper_sha256' PLOYZ_NATS_URL='$edge_runtime_nats_url' PLOYZ_NODE_PUBLIC_IP='$edge_ip' sh '$remote_ployz_sh' --join-token '$join_token'"

    remote_sh "$edge_ip" "if [ -f '$remote_bootstrap_tunnel_pid' ]; then kill \"\$(cat '$remote_bootstrap_tunnel_pid')\" >/dev/null 2>&1 || true; fi"

    wait_for_machine_add_operation "$core_ip" op_machine_add

    wait_for_machine_ready "$core_ip" edge_2 "$edge_ip"

    run_remote_logged inspect-edge-through-runtime-tunnel "$edge_ip" \
      "PLOYZ_NATS_URL='$edge_runtime_nats_url' '$remote_ployzctl' machine inspect edge_2"

    run_remote_logged deploy-smoke "$core_ip" \
      "PLOYZ_NATS_URL=nats://127.0.0.1:4222 '$remote_ployzctl' deploy --detach --service svc_smoke --revision rev_smoke_1 --image '$smoke_image' --replicas 1 --operation op_deploy_smoke --idempotency-key idem_deploy_smoke --route-hostname smoke.local --route-port 8080 --endpoint-port 80"

    wait_for_deploy_operation "$core_ip" op_deploy_smoke

    echo "curling smoke service through core gateway" >&2
    wait_for_smoke_service core "$core_ip"
    echo "curling smoke service through edge gateway" >&2
    wait_for_smoke_service edge "$edge_ip"

    if [ "${PLOYZ_ACCEPTANCE_KEEP:-0}" != "1" ]; then
      cleanup_servers
      echo "disposable host cleanup complete"
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
