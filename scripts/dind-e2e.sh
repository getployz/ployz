#!/usr/bin/env bash
set -euo pipefail

# Dev-loop wrapper for the Docker-in-Docker e2e suite
# (docs/operations/dind-e2e.md, plan C5).
#
# Rebuilds changed linux artifacts, then runs the smoke test serially and the
# compatible scenario groups concurrently, then the heavyweight acceptance
# lifecycle serially:
#
#   scripts/dind-e2e.sh                              # clean gated suite
#   scripts/dind-e2e.sh scenario_machine_add         # one scenario (filter)
#   scripts/dind-e2e.sh --full scenario_machine_add  # clean one scenario
#
# Leftover Docker resources from crashed runs: scripts/dind-clean.sh.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

TARGET_DIR="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
ARTIFACT_DIR="${TARGET_DIR}/release"
MACHINE_IMAGE="${PLOYZ_DIND_MACHINE_IMAGE:-ployz-dind-machine:local}"
WORKERS="${PLOYZ_DIND_WORKERS:-2}"
TEST_STACK_BYTES=16777216

case "${WORKERS}" in
  1 | 2 | 3) ;;
  *)
    echo "PLOYZ_DIND_WORKERS must be 1, 2, or 3" >&2
    exit 1
    ;;
esac

command -v docker >/dev/null 2>&1 || {
  echo "docker is required for the DinD e2e suite" >&2
  exit 1
}

platform="$(docker_platform "${PLOYZ_DIND_PLATFORM:-}")"
build_mode=full
incremental=0

if [ "${1:-}" = "--full" ]; then
  shift
elif [ "$#" -gt 0 ]; then
  fingerprint="$(PLOYZ_DIND_PLATFORM="${platform}" bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh" fingerprint)"
  image_identity="$(docker image inspect --format '{{.Os}}/{{.Architecture}} {{index .Config.Labels "dev.ployz.dind.fingerprint"}}' "${MACHINE_IMAGE}" 2>/dev/null || true)"
  if [ "${image_identity}" = "${platform} ${fingerprint}" ]; then
    build_mode=artifacts-only
    incremental=1
  fi
fi

PLOYZ_DIND_TARGET_DIR="${TARGET_DIR}" \
  PLOYZ_DIND_MACHINE_IMAGE="${MACHINE_IMAGE}" \
  PLOYZ_DIND_PLATFORM="${platform}" \
  PLOYZ_DIND_CARGO_INCREMENTAL="${incremental}" \
  bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh" "${build_mode}"

cd "${ROOT_DIR}"

run_dind() {
  local workers="$1"
  shift
  PLOYZ_DIND_E2E=1 \
    PLOYZ_DIND_MACHINE_IMAGE="${MACHINE_IMAGE}" \
    PLOYZ_DIND_ARTIFACT_DIR="${ARTIFACT_DIR}" \
    PLOYZ_DIND_PLATFORM="${platform}" \
    RUST_MIN_STACK="${TEST_STACK_BYTES}" \
    cargo test -p ployz-e2e --test dind_cluster -- --test-threads="${workers}" --nocapture "$@"
}

if [ "$#" -gt 0 ]; then
  run_dind "${WORKERS}" "$@"
else
  run_dind 1 serial_smoke --exact
  run_dind "${WORKERS}" group_ --skip acceptance::group_v1_acceptance
  run_dind 1 acceptance::group_v1_acceptance --exact
fi
