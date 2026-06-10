#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${PLOYZ_LOCAL_DATAPLANE_IMAGE:-rust:1.91-bookworm}"
PROOF_IMAGE="${PLOYZ_LOCAL_DATAPLANE_PROOF_IMAGE:-ployz-local-dataplane-proof:rust-1.91-bookworm-v5}"
NATS_SERVER_VERSION="${PLOYZ_LOCAL_DATAPLANE_NATS_SERVER_VERSION:-2.14.2}"
TARGET_DIR="${PLOYZ_LOCAL_DATAPLANE_TARGET_DIR:-/tmp/ployz-local-dataplane-target}"
EBPF_TARGET_DIR="${PLOYZ_LOCAL_DATAPLANE_EBPF_TARGET_DIR:-/tmp/ployz-local-dataplane-ebpf-target}"
CARGO_REGISTRY_DIR="${PLOYZ_LOCAL_DATAPLANE_CARGO_REGISTRY_DIR:-/tmp/ployz-local-dataplane-cargo-registry}"
CARGO_GIT_DIR="${PLOYZ_LOCAL_DATAPLANE_CARGO_GIT_DIR:-/tmp/ployz-local-dataplane-cargo-git}"
CONTAINER_NAME="ployz-local-dataplane-proof-$$"

command -v docker >/dev/null 2>&1 || {
  echo "docker is required for the local dataplane proof" >&2
  exit 1
}

docker_platform() {
  if [ -n "${PLOYZ_LOCAL_DATAPLANE_PLATFORM:-}" ]; then
    printf '%s\n' "${PLOYZ_LOCAL_DATAPLANE_PLATFORM}"
    return 0
  fi

  case "$(docker info --format '{{.Architecture}}')" in
    aarch64 | arm64)
      printf 'linux/arm64\n'
      ;;
    x86_64 | amd64)
      printf 'linux/amd64\n'
      ;;
    *)
      docker info --format 'linux/{{.Architecture}}'
      ;;
  esac
}

cleanup() {
  docker rm -f ployz-local-core ployz-local-edge >/dev/null 2>&1 || true
  docker network rm ployz-local-dind-proof >/dev/null 2>&1 || true
  docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

build_proof_image() {
  if docker image inspect "${PROOF_IMAGE}" >/dev/null 2>&1; then
    return 0
  fi

  docker build \
    --platform "$(docker_platform)" \
    --tag "${PROOF_IMAGE}" \
    - <<DOCKERFILE
FROM ${IMAGE}
ENV PATH="/usr/local/cargo/bin:\${PATH}"
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \\
  && apt-get install -y --no-install-recommends \\
    ca-certificates \\
    clang \\
    curl \\
    docker.io \\
    dbus \\
    iproute2 \\
    iptables \\
    lld \\
    llvm \\
    pkg-config \\
    systemd \\
    systemd-sysv \\
    wireguard-tools \\
  && rm -rf /var/lib/apt/lists/*
RUN arch="\$(dpkg --print-architecture)" \\
  && case "\${arch}" in amd64) nats_arch=amd64 ;; arm64) nats_arch=arm64 ;; *) echo "unsupported NATS arch: \${arch}" >&2; exit 1 ;; esac \\
  && curl -fsSL -o /tmp/nats-server.tar.gz "https://github.com/nats-io/nats-server/releases/download/v${NATS_SERVER_VERSION}/nats-server-v${NATS_SERVER_VERSION}-linux-\${nats_arch}.tar.gz" \\
  && mkdir -p /tmp/nats-server \\
  && tar -xzf /tmp/nats-server.tar.gz -C /tmp/nats-server --strip-components=1 \\
  && install -m 0755 /tmp/nats-server/nats-server /usr/local/bin/nats-server \\
  && rm -rf /tmp/nats-server /tmp/nats-server.tar.gz
RUN rustup install nightly \\
  && rustup component add rust-src --toolchain nightly \\
  && cargo +nightly install --locked bpf-linker
DOCKERFILE
}

mkdir -p "${TARGET_DIR}" "${EBPF_TARGET_DIR}" "${CARGO_REGISTRY_DIR}" "${CARGO_GIT_DIR}"
build_proof_image

docker run \
  --name "${CONTAINER_NAME}" \
  --detach \
  --platform "$(docker_platform)" \
  --privileged \
  --workdir /work \
  --volume "${ROOT_DIR}:/work" \
  --volume "${TARGET_DIR}:${TARGET_DIR}" \
  --volume "${EBPF_TARGET_DIR}:${EBPF_TARGET_DIR}" \
  --volume "${CARGO_REGISTRY_DIR}:/usr/local/cargo/registry" \
  --volume "${CARGO_GIT_DIR}:/usr/local/cargo/git" \
  --tmpfs /run \
  --tmpfs /run/lock \
  "${PROOF_IMAGE}" \
  sleep infinity >/dev/null

docker exec "${CONTAINER_NAME}" bash -lc '
set -euo pipefail
export PATH="/usr/local/cargo/bin:${PATH}"
nohup dockerd --host=unix:///var/run/docker.sock --storage-driver=vfs >/tmp/ployz-dockerd.log 2>&1 &
for _ in $(seq 1 60); do
  if docker info >/dev/null 2>&1; then
    exit 0
  fi
  sleep 1
done
cat /tmp/ployz-dockerd.log >&2 || true
exit 1
'

docker exec "${CONTAINER_NAME}" bash -lc '
set -euo pipefail

export PATH="/usr/local/cargo/bin:${PATH}"
docker info >/dev/null

mountpoint -q /sys/fs/bpf || mount -t bpf bpf /sys/fs/bpf

export CARGO_TARGET_DIR='"${TARGET_DIR}"'
export PLOYZ_EBPF_TARGET_DIR='"${EBPF_TARGET_DIR}"'
cargo build -p ployzd -p ployzctl -p ployz-keeper -p ployz-ebpf-ctl
ebpf_bytecode="$(scripts/build-ebpf-bytecode.sh | tail -n 1)"
printf "%s\n" "${ebpf_bytecode}" > "${CARGO_TARGET_DIR}/ployz-ebpf-bytecode.path"

PLOYZ_LOCAL_DATAPLANE_PROOF=1 \
PLOYZ_LOCAL_DATAPLANE_EBPF_CTL="${CARGO_TARGET_DIR}/debug/ployz-ebpf-ctl" \
PLOYZ_LOCAL_DATAPLANE_EBPF_BYTECODE="${ebpf_bytecode}" \
cargo test -p ployzd --test wireguard_dataplane -- --test-threads=1 --nocapture
'

ebpf_bytecode="$(cat "${TARGET_DIR}/ployz-ebpf-bytecode.path")"
proof_net="ployz-local-dind-proof"
core_machine="ployz-local-core"
edge_machine="ployz-local-edge"
route_port=8080
endpoint_port=80
smoke_image="nginx:1.27-alpine"
core_host_port=19080
edge_host_port=19081
proof_image="${PROOF_IMAGE}"
ployzd="${TARGET_DIR}/debug/ployzd"
ployzctl="${TARGET_DIR}/debug/ployzctl"
keeper="${TARGET_DIR}/debug/ployz-keeper"
ebpf_ctl="${TARGET_DIR}/debug/ployz-ebpf-ctl"
nats_server="/usr/local/bin/nats-server"

docker rm -f "${core_machine}" "${edge_machine}" >/dev/null 2>&1 || true
docker network rm "${proof_net}" >/dev/null 2>&1 || true
docker network create "${proof_net}" >/dev/null

start_machine() {
  local name="$1"
  local host_port="$2"
  docker run \
    --detach \
    --privileged \
    --cgroupns=host \
    --network "${proof_net}" \
    --name "${name}" \
    --hostname "${name}" \
    --platform "$(docker_platform)" \
    --publish "127.0.0.1:${host_port}:${route_port}" \
    --stop-signal SIGRTMIN+3 \
    --volume "${ROOT_DIR}:/work" \
    --volume "${TARGET_DIR}:${TARGET_DIR}:ro" \
    --volume "${EBPF_TARGET_DIR}:${EBPF_TARGET_DIR}:ro" \
    --volume /sys/fs/cgroup:/sys/fs/cgroup:rw \
    --tmpfs /run \
    --tmpfs /run/lock \
    "${proof_image}" \
    /sbin/init >/dev/null
  for _ in $(seq 1 120); do
    if [ "$(docker inspect -f '{{.State.Running}}' "${name}" 2>/dev/null || true)" != "true" ]; then
      docker inspect "${name}" >&2 || true
      docker logs "${name}" >&2 || true
      echo "${name} exited before systemd became ready" >&2
      return 1
    fi
    state="$(docker exec "${name}" systemctl is-system-running 2>/dev/null || true)"
    if printf '%s\n' "${state}" | grep -Eq "^(running|degraded)$"; then
      break
    fi
    sleep 0.5
  done
  if [ "$(docker inspect -f '{{.State.Running}}' "${name}" 2>/dev/null || true)" != "true" ]; then
    docker inspect "${name}" >&2 || true
    docker logs "${name}" >&2 || true
    echo "${name} exited before docker could start" >&2
    return 1
  fi
  docker exec "${name}" systemctl start docker
  for _ in $(seq 1 120); do
    if docker exec "${name}" docker info >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  docker exec "${name}" systemctl status docker --no-pager >&2 || true
  docker exec "${name}" journalctl -u docker --no-pager -n 80 >&2 || true
  echo "${name} dockerd did not become ready" >&2
  return 1
}

write_join_template() {
  local runtime_nats_url="$1"
  local ployzd_sha="$2"
  local ebpf_bytecode_sha="$3"
  local ebpf_ctl_sha="$4"
  docker exec -i "${core_machine}" bash -lc "cat > /tmp/ployz-join-template.json" <<JSON
{
  "join_bundle": {
    "material": {
      "cluster_name": "local-dind",
      "runtime_nats_url": "${runtime_nats_url}",
      "trusted_nats": {
        "server_id": "server_1",
        "config_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      },
      "core_iroh": {
        "node_id": "core_1",
        "public_key": "core-public-key",
        "direct_addresses": [],
        "relay_url": null
      },
      "ployzd": {
        "version": "local",
        "source": "${ployzd}",
        "sha256": "${ployzd_sha}",
        "install_path": "/usr/local/bin/ployzd"
      },
      "ebpf_bytecode": {
        "version": "local",
        "source": "${ebpf_bytecode}",
        "sha256": "${ebpf_bytecode_sha}",
        "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
      },
      "ebpf_ctl": {
        "version": "local",
        "source": "${ebpf_ctl}",
        "sha256": "${ebpf_ctl_sha}",
        "install_path": "/usr/local/bin/ployz-ebpf-ctl"
      }
    }
  },
  "secret_delivery": {
    "nats_credentials": "user-jwt-and-seed",
    "core_iroh_ticket": "core-ticket"
  }
}
JSON
  docker exec "${core_machine}" install -d -m 0755 /etc/ployz
  docker exec "${core_machine}" install -m 0644 /tmp/ployz-join-template.json /etc/ployz/machine-join-template.json
}

machine_ip() {
  docker exec "$1" bash -lc "hostname -i | sed \"s/ .*//\""
}

start_nats_and_control() {
  local core_ip="$1"
  local core_nats_url="nats://${core_ip}:4222"
  local ployzd_sha
  local keeper_sha
  local ebpf_bytecode_sha
  local ebpf_ctl_sha
  local nats_sha
  ployzd_sha="$(docker exec "${core_machine}" sha256sum "${ployzd}" | cut -d" " -f1)"
  keeper_sha="$(docker exec "${core_machine}" sha256sum "${keeper}" | cut -d" " -f1)"
  ebpf_bytecode_sha="$(docker exec "${core_machine}" sha256sum "${ebpf_bytecode}" | cut -d" " -f1)"
  ebpf_ctl_sha="$(docker exec "${core_machine}" sha256sum "${ebpf_ctl}" | cut -d" " -f1)"
  nats_sha="$(docker exec "${core_machine}" sha256sum "${nats_server}" | cut -d" " -f1)"
  write_join_template "${core_nats_url}" "${ployzd_sha}" "${ebpf_bytecode_sha}" "${ebpf_ctl_sha}"
  docker exec "${core_machine}" bash -lc "${ployzctl} init --run-keeper-install --node core_1 --gateway --keeper-binary ${keeper} --ployzd-version local --ployzd-source ${ployzd} --ployzd-sha256 ${ployzd_sha} --ployzd-install-path /usr/local/bin/ployzd --ebpf-bytecode-version local --ebpf-bytecode-source ${ebpf_bytecode} --ebpf-bytecode-sha256 ${ebpf_bytecode_sha} --ebpf-bytecode-install-path /usr/local/lib/ployz/ebpf/ployz-ebpf-tc --ebpf-ctl-version local --ebpf-ctl-source ${ebpf_ctl} --ebpf-ctl-sha256 ${ebpf_ctl_sha} --ebpf-ctl-install-path /usr/local/bin/ployz-ebpf-ctl --nats-version local --nats-source ${nats_server} --nats-sha256 ${nats_sha} --nats-binary /usr/local/bin/nats-server --nats-config /etc/nats/nats-server.conf --machine-bootstrap-url https://local.invalid/ployz.sh --machine-join-template-file /etc/ployz/machine-join-template.json --node-public-ip ${core_ip} >/tmp/ployz-init.log 2>&1"
  docker exec "${core_machine}" bash -lc "sed -i 's/^host: 127.0.0.1$/host: 0.0.0.0/' /etc/nats/nats-server.conf && systemctl restart nats-server ployzd-control ployzd-node-core_1 ployzd-gateway"
  for _ in $(seq 1 120); do
    if docker exec "${core_machine}" bash -lc "${ployzctl} --nats nats://127.0.0.1:4222 machine list >/tmp/ployz-machine-list.log 2>&1"; then
      docker exec "${core_machine}" bash -lc "${ployzctl} --nats nats://127.0.0.1:4222 init activate-first-node --node core_1 --gateway >/tmp/ployz-activate.log 2>&1"
      return 0
    fi
    sleep 0.5
  done
  docker exec "${core_machine}" cat /tmp/ployz-init.log >&2 || true
  docker exec "${core_machine}" cat /etc/ployz/ployzd.env >&2 || true
  docker exec "${core_machine}" systemctl status nats-server ployzd-control ployzd-node-core_1 ployzd-gateway --no-pager >&2 || true
  docker exec "${core_machine}" journalctl -u ployzd-control --no-pager -n 120 >&2 || true
  docker exec "${core_machine}" cat /tmp/ployz-machine-list.log >&2 || true
  echo "control API did not become ready" >&2
  return 1
}

join_edge_machine() {
  local edge_ip="$1"
  local join_token
  local keeper_sha
  join_token="$(docker exec "${core_machine}" bash -lc "${ployzctl} --nats nats://127.0.0.1:4222 machine add --node edge_2 --name edge-2 --gateway --operation op_add_edge_2 --idempotency-key idem_add_edge_2" | awk '/^join-token / {print $2}')"
  test -n "${join_token}"
  keeper_sha="$(docker exec "${edge_machine}" sha256sum "${keeper}" | cut -d" " -f1)"
  if ! docker exec "${edge_machine}" bash -lc "PLOYZ_KEEPER_URL=file://${keeper} PLOYZ_KEEPER_SHA256=${keeper_sha} PLOYZ_NATS_URL=nats://$(machine_ip "${core_machine}"):4222 PLOYZ_NODE_PUBLIC_IP=${edge_ip} sh /work/scripts/ployz.sh --join-token ${join_token} >/tmp/ployz-edge-join.log 2>&1"; then
    docker exec "${edge_machine}" cat /tmp/ployz-edge-join.log >&2 || true
    docker exec "${edge_machine}" systemctl status ployzd-node-edge_2 ployzd-gateway ployzd-tunnel-edge --no-pager >&2 || true
    docker exec "${edge_machine}" journalctl -u ployzd-node-edge_2 -u ployzd-gateway -u ployzd-tunnel-edge --no-pager -n 160 >&2 || true
    echo "edge bootstrap command failed" >&2
    return 1
  fi
}

prepare_machine_dataplane() {
  local machine="$1"
  docker exec "${machine}" bash -lc "mountpoint -q /sys/fs/bpf || mount -t bpf bpf /sys/fs/bpf"
}

wait_for_processes_to_publish() {
  for _ in $(seq 1 120); do
    if docker exec "${core_machine}" bash -lc "${ployzctl} --nats nats://127.0.0.1:4222 machine inspect edge_2 | grep -q \"containers 0\"" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  docker exec "${edge_machine}" cat /tmp/ployz-edge-join.log >&2 || true
  docker exec "${edge_machine}" systemctl status ployzd-node-edge_2 ployzd-gateway --no-pager >&2 || true
  echo "edge machine did not publish observations" >&2
  return 1
}

assert_gateway() {
  local host_port="$1"
  local response
  response="$(curl -fsS --max-time 10 -H "Host: smoke.local:${route_port}" "http://127.0.0.1:${host_port}/")"
  printf '%s\n' "${response}" | grep -q "Welcome to nginx"
}

start_machine "${core_machine}" "${core_host_port}"
start_machine "${edge_machine}" "${edge_host_port}"
prepare_machine_dataplane "${core_machine}"
prepare_machine_dataplane "${edge_machine}"
core_ip="$(machine_ip "${core_machine}")"
edge_ip="$(machine_ip "${edge_machine}")"
start_nats_and_control "${core_ip}"
join_edge_machine "${edge_ip}"
wait_for_processes_to_publish

docker exec "${core_machine}" bash -lc "${ployzctl} --nats nats://127.0.0.1:4222 deploy --detach --service svc_smoke --revision rev_local --image ${smoke_image} --replicas 2 --operation op_local_dind --idempotency-key idem_local_dind --route-hostname smoke.local --route-port ${route_port} --endpoint-port ${endpoint_port}"
docker exec "${core_machine}" bash -lc "${ployzctl} --nats nats://127.0.0.1:4222 ops watch op_local_dind --json" | tee /tmp/ployz-local-dind-events.jsonl
docker exec "${core_machine}" bash -lc "${ployzctl} --nats nats://127.0.0.1:4222 ops status op_local_dind" | tee /tmp/ployz-local-dind-status.txt
grep -q "state completed" /tmp/ployz-local-dind-status.txt
grep -q "deploy.wireguard_ebpf_prepared" /tmp/ployz-local-dind-events.jsonl
assert_gateway "${core_host_port}"
assert_gateway "${edge_host_port}"
echo "local DIND two-machine deploy proof passed"
