#!/usr/bin/env bash
set -euo pipefail

# Build the x86_64 machine artifacts, test the role-neutral harness, then run
# each gated v2 public-seam proof exactly once.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"
cd "${ROOT_DIR}"

platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-linux/amd64}")"
if [ "${platform}" != "linux/amd64" ]; then
  echo "Keeper mesh DinD proof supports linux/amd64 only, got ${platform}" >&2
  exit 1
fi
export PLOYZ_DIND_PLATFORM="${platform}"

scripts/build-dind-machine-image.sh full
cargo test -p ployz-e2e --lib
cargo test -p ployz-e2e --tests --no-run

jobs="${PLOYZ_DIND_JOBS:-$(nproc)}"
case "${jobs}" in
  ''|*[!0-9]*|0)
    echo "PLOYZ_DIND_JOBS must be a positive integer, got ${jobs}" >&2
    exit 1
    ;;
esac
if [ "${jobs}" -gt 4 ]; then
  jobs=4
fi

# Longest scenarios start first so a bounded worker pool approaches the
# duration of the slowest product proof instead of the sum of every proof.
scenarios=(
  operation_placement
  token_door_join
  operation_deploy
  gateway_routes
  container_plane
  init_machine_one
  keeper_mesh
  laptop_dial
)

declare -A running=()
active=0
failed=0

wait_for_scenario() {
  local completed_pid='' outcome
  if wait -n -p completed_pid; then
    outcome=passed
  else
    failed=1
    outcome=failed
  fi
  if [ -n "${completed_pid}" ]; then
    echo "DinD scenario ${running[${completed_pid}]} ${outcome}"
    unset 'running['"${completed_pid}"']'
  fi
  active=$((active - 1))
}

for scenario in "${scenarios[@]}"; do
  while [ "${active}" -ge "${jobs}" ]; do
    wait_for_scenario
  done
  (
    set -o pipefail
    PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test "${scenario}" "$@" -- --nocapture 2>&1 \
      | sed -u "s/^/[${scenario}] /"
  ) &
  pid=$!
  running["${pid}"]="${scenario}"
  active=$((active + 1))
done

while [ "${active}" -gt 0 ]; do
  wait_for_scenario
done

exit "${failed}"
