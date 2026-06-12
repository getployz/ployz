#!/usr/bin/env bash
# Shared helpers for the ployz scripts. Source this file; do not execute it.

# The workspace crates that produce the linux binaries the harnesses stage
# onto machines. scripts/build-dind-machine-image.sh builds exactly these
# (plus the ployz-ebpf-tc bytecode) into its release artifact dir.
PLOYZ_BINARY_CRATES=(ployzd ployzctl ployz-keeper ployz-ebpf-ctl)

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
