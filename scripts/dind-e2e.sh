#!/usr/bin/env bash
set -euo pipefail

# Dev-loop wrapper for the Docker-in-Docker e2e suite
# (docs/operations/dind-e2e.md, plan C5).
#
# Rebuilds the linux artifacts + machine image only when stale — a marker
# file records the binary hashes/mtimes, the image id, and the newest
# source mtime from the last build — then runs the gated suite serialized:
#
#   scripts/dind-e2e.sh                              # full suite
#   scripts/dind-e2e.sh scenario_machine_add         # one scenario (filter)
#   scripts/dind-e2e.sh --check-stale                # report staleness only;
#                                                    # exits 1 when stale
#
# Leftover Docker resources from crashed runs: scripts/dind-clean.sh.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib.sh
source "${ROOT_DIR}/scripts/lib.sh"

TARGET_DIR="${PLOYZ_DIND_TARGET_DIR:-/tmp/ployz-dind-machine-target}"
ARTIFACT_DIR="${TARGET_DIR}/release"
MACHINE_IMAGE="${PLOYZ_DIND_MACHINE_IMAGE:-ployz-dind-machine:local}"
MARKER_FILE="${TARGET_DIR}/.dind-e2e-build-marker"
BINARIES=("${PLOYZ_BINARY_CRATES[@]}" ployz-ebpf-tc)

CHECK_STALE=0
if [ "${1:-}" = "--check-stale" ]; then
  CHECK_STALE=1
  shift
fi

command -v docker >/dev/null 2>&1 || {
  echo "docker is required for the DinD e2e suite" >&2
  exit 1
}

if [ "$(uname)" = "Darwin" ]; then
  mtime_of() { stat -f %m "$1"; }
  mtimes_of_stdin0() { xargs -0 stat -f %m 2>/dev/null || true; }
else
  mtime_of() { stat -c %Y "$1"; }
  mtimes_of_stdin0() { xargs -0 stat -c %Y 2>/dev/null || true; }
fi

# Newest mtime across the inputs that feed the artifacts/image: workspace
# sources and the image build context (tracked + untracked, gitignore
# respected — nginx.tar is regenerated every build and must stay excluded).
newest_source_mtime() {
  git -C "${ROOT_DIR}" ls-files -z --cached --others --exclude-standard -- \
    'crates' 'Cargo.toml' 'Cargo.lock' 'docker/dind-machine' \
    'scripts/build-dind-machine-image.sh' 'scripts/build-ebpf-bytecode.sh' \
    'scripts/lib.sh' |
    (cd "${ROOT_DIR}" && mtimes_of_stdin0) | sort -n | tail -n 1
}

# What the last build produced and what it was built from. Any change —
# a touched binary (mtime is part of the fingerprint), a rebuilt/removed
# image, or an edited source file — makes the marker mismatch, i.e. stale.
# The source mtime lives in the marker (not compared against binary mtimes)
# because cargo legitimately skips relinking unchanged binaries.
fingerprint() {
  local binary
  for binary in "${BINARIES[@]}"; do
    printf '%s %s %s\n' \
      "$(sha256_of "${ARTIFACT_DIR}/${binary}")" \
      "$(mtime_of "${ARTIFACT_DIR}/${binary}")" \
      "${binary}"
  done
  printf 'image %s\n' \
    "$(docker image inspect --format '{{.Id}}' "${MACHINE_IMAGE}" 2>/dev/null || echo absent)"
  printf 'sources %s\n' "$(newest_source_mtime)"
}

# Prints the staleness reason and returns 0 when a rebuild is needed;
# returns 1 (prints nothing) when artifacts and image are fresh.
stale_reason() {
  local binary
  for binary in "${BINARIES[@]}"; do
    if [ ! -f "${ARTIFACT_DIR}/${binary}" ]; then
      echo "missing artifact ${ARTIFACT_DIR}/${binary}"
      return 0
    fi
  done
  if ! docker image inspect "${MACHINE_IMAGE}" >/dev/null 2>&1; then
    echo "machine image ${MACHINE_IMAGE} not built"
    return 0
  fi
  if [ ! -f "${MARKER_FILE}" ]; then
    echo "no build marker at ${MARKER_FILE}"
    return 0
  fi
  if [ "$(fingerprint)" != "$(cat "${MARKER_FILE}")" ]; then
    echo "artifact/source fingerprint changed since last build"
    return 0
  fi
  return 1
}

if reason="$(stale_reason)"; then
  if [ "${CHECK_STALE}" = "1" ]; then
    echo "stale: ${reason}"
    exit 1
  fi
  echo "rebuilding artifacts + machine image: ${reason}"
  PLOYZ_DIND_TARGET_DIR="${TARGET_DIR}" \
    PLOYZ_DIND_MACHINE_IMAGE="${MACHINE_IMAGE}" \
    bash "${ROOT_DIR}/scripts/build-dind-machine-image.sh"
  fingerprint >"${MARKER_FILE}"
else
  if [ "${CHECK_STALE}" = "1" ]; then
    echo "fresh"
    exit 0
  fi
  echo "artifacts and machine image fresh; skipping build"
fi

cd "${ROOT_DIR}"
PLOYZ_DIND_E2E=1 \
  PLOYZ_DIND_MACHINE_IMAGE="${MACHINE_IMAGE}" \
  PLOYZ_DIND_ARTIFACT_DIR="${ARTIFACT_DIR}" \
  cargo test -p ployz-e2e --test dind_cluster -- --test-threads=1 --nocapture "$@"
