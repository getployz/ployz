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
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test keeper_mesh "$@" -- --nocapture
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test container_plane "$@" -- --nocapture
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test operation_deploy "$@" -- --nocapture
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test operation_placement "$@" -- --nocapture
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test gateway_routes "$@" -- --nocapture
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test init_machine_one "$@" -- --nocapture
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test laptop_dial "$@" -- --nocapture
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test token_door_join "$@" -- --nocapture
