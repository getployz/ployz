#!/usr/bin/env bash
# Shared helpers for the ployz scripts. Source this file; do not execute it.

# The workspace crates that produce the linux binaries the harnesses stage
# onto machines. scripts/build-dind-machine-image.sh builds exactly these
# (plus the ployz-ebpf-tc bytecode) into its release artifact dir.
PLOYZ_BINARY_CRATES=(ployzd ployz ployz-ebpf-ctl)

# The host docker server architecture as a --platform value — never hardcode
# amd64. An optional first argument (a caller-specific override env var)
# wins when non-empty.
docker_platform() {
  if [ -n "${1:-}" ]; then
    printf '%s\n' "$1"
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

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | cut -d' ' -f1
  else
    shasum -a 256 | cut -d' ' -f1
  fi
}

verify_corrosion_embedded_version() {
  local platform="$1" work_dir="$2" runtime_image="$3" expected_version="$4"
  local actual_version
  actual_version="$(docker run --rm \
    --platform "${platform}" \
    --volume "${work_dir}:/corrosion-input:ro" \
    "${runtime_image}" \
    /corrosion-input/corrosion --version)"
  if [ "${actual_version}" != "${expected_version}" ]; then
    echo "Corrosion archive reports ${actual_version}, expected ${expected_version}" >&2
    return 1
  fi
}

validate_release_semver() {
  local label="$1"
  local value="$2"
  local semver="$3"

  if [ -z "${semver}" ]; then
    echo "${label} must include a version after v: ${value}" >&2
    exit 1
  fi

  local version_core major minor minor_patch patch
  version_core="${semver%%-*}"
  major="${version_core%%.*}"
  minor_patch="${version_core#*.}"
  if [ "${minor_patch}" = "${version_core}" ]; then
    echo "${label} must look like vX.Y.Z or vX.Y.Z-suffix: ${value}" >&2
    exit 1
  fi
  patch="${minor_patch#*.}"
  if [ "${patch}" = "${minor_patch}" ]; then
    echo "${label} must look like vX.Y.Z or vX.Y.Z-suffix: ${value}" >&2
    exit 1
  fi
  minor="${minor_patch%%.*}"
  case "${major}:${minor}:${patch}" in
    *[!0-9.:]* | :* | *::*)
      echo "${label} must look like vX.Y.Z or vX.Y.Z-suffix: ${value}" >&2
      exit 1
      ;;
  esac
}
