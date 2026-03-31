#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

root_version="$(bash "${ROOT_DIR}/scripts/read-workspace-version.sh")"
ebpf_version="$(
  awk '
    /^\[package\]$/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^version = "/ {
      line = $0
      sub(/^version = "/, "", line)
      sub(/"$/, "", line)
      print line
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "${ROOT_DIR}/ebpf/Cargo.toml"
)"

if [[ "${root_version}" != "${ebpf_version}" ]]; then
  printf 'ebpf/Cargo.toml version (%s) does not match root workspace version (%s)\n' "${ebpf_version}" "${root_version}" >&2
  exit 1
fi
