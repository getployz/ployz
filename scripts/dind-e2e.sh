#!/usr/bin/env bash
set -euo pipefail

# Dev-loop wrapper for the Docker-in-Docker e2e suite
# (docs/operations/dind-e2e.md, plan C5).
#
# Rebuilds the linux artifacts + machine image, then runs the gated suite
# serialized:
#
#   scripts/dind-e2e.sh                              # full suite
#   scripts/dind-e2e.sh scenario_machine_add         # one scenario (filter)
#
# Leftover Docker resources from crashed runs: scripts/dind-clean.sh.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TARGET_DIR="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
ARTIFACT_DIR="${TARGET_DIR}/release"
MACHINE_IMAGE="${PLOYZ_DIND_MACHINE_IMAGE:-ployz-dind-machine:local}"

command -v docker >/dev/null 2>&1 || {
  echo "docker is required for the DinD e2e suite" >&2
  exit 1
}

PLOYZ_DIND_TARGET_DIR="${TARGET_DIR}" \
  PLOYZ_DIND_MACHINE_IMAGE="${MACHINE_IMAGE}" \
  bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh"

cd "${ROOT_DIR}"
PLOYZ_DIND_E2E=1 \
  PLOYZ_DIND_MACHINE_IMAGE="${MACHINE_IMAGE}" \
  PLOYZ_DIND_ARTIFACT_DIR="${ARTIFACT_DIR}" \
  cargo test -p ployz-e2e --test dind_cluster -- --test-threads=1 --nocapture "$@"
