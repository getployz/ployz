#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${PLOYZ_ACCEPTANCE_TARGET_DIR:-${CARGO_TARGET_DIR:-/tmp/ployz-build-target}}"

need_file() {
  local path=$1
  local label=$2
  [[ -f "${path}" ]] || {
    echo "${label} does not exist: ${path}" >&2
    exit 1
  }
}

resolve_nats_server() {
  if [[ -n "${PLOYZ_ACCEPTANCE_NATS_SERVER:-}" ]]; then
    printf '%s\n' "${PLOYZ_ACCEPTANCE_NATS_SERVER}"
    return
  fi
  if command -v nats-server >/dev/null 2>&1; then
    command -v nats-server
    return
  fi
  echo "set PLOYZ_ACCEPTANCE_NATS_SERVER or install nats-server on PATH" >&2
  exit 1
}

resolve_ebpf_bytecode() {
  if [[ -n "${PLOYZ_ACCEPTANCE_EBPF_BYTECODE:-}" ]]; then
    printf '%s\n' "${PLOYZ_ACCEPTANCE_EBPF_BYTECODE}"
    return
  fi
  "${ROOT_DIR}/scripts/install-ebpf-bytecode.sh" | tail -n 1
}

cargo build \
  --target-dir "${TARGET_DIR}" \
  -p ployzctl \
  -p ployzd \
  -p ployz-keeper \
  -p ployz-ebpf-ctl

ployzctl_bin="${TARGET_DIR}/debug/ployzctl"
ployzd_bin="${TARGET_DIR}/debug/ployzd"
keeper_bin="${TARGET_DIR}/debug/ployz-keeper"
ebpf_ctl_bin="${TARGET_DIR}/debug/ployz-ebpf-ctl"
nats_server_bin="$(resolve_nats_server)"
ebpf_bytecode="$(resolve_ebpf_bytecode)"

need_file "${ployzctl_bin}" "ployzctl"
need_file "${ployzd_bin}" "ployzd"
need_file "${keeper_bin}" "ployz-keeper"
need_file "${ebpf_ctl_bin}" "ployz-ebpf-ctl"
need_file "${nats_server_bin}" "nats-server"
need_file "${ebpf_bytecode}" "eBPF bytecode"

cat <<EOF
export PLOYZ_ACCEPTANCE_TARGET_DIR="${TARGET_DIR}"
export PLOYZ_ACCEPTANCE_PLOYZCTL="${ployzctl_bin}"
export PLOYZ_ACCEPTANCE_PLOYZD="${ployzd_bin}"
export PLOYZ_ACCEPTANCE_KEEPER="${keeper_bin}"
export PLOYZ_ACCEPTANCE_EBPF_CTL="${ebpf_ctl_bin}"
export PLOYZ_ACCEPTANCE_EBPF_BYTECODE="${ebpf_bytecode}"
export PLOYZ_ACCEPTANCE_NATS_SERVER="${nats_server_bin}"

scripts/hetzner-two-node-acceptance.sh up --run-id <id>
EOF
