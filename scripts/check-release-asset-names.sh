#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

check_asset_name() {
  local target_platform=$1
  local expected=$2
  local actual

  actual="$(bash "${ROOT_DIR}/ployz.sh" internal payload-asset-name --target-platform "${target_platform}")"
  if [[ "${actual}" != "${expected}" ]]; then
    printf 'asset mismatch for %s: expected %s, got %s\n' "${target_platform}" "${expected}" "${actual}" >&2
    exit 1
  fi
}

check_asset_name "linux/amd64" "ployz-payload-linux-x86_64.tar.gz"
check_asset_name "linux/arm64" "ployz-payload-linux-aarch64.tar.gz"
check_asset_name "darwin/amd64" "ployz-payload-darwin-x86_64.tar.gz"
check_asset_name "darwin/arm64" "ployz-payload-darwin-aarch64.tar.gz"

printf 'release asset names ok\n'
