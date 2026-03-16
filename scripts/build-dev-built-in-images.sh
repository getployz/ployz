#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_path="${1:-$repo_dir/target/ployz-dev/built-in-images.toml}"

log() {
  if [[ -w /dev/tty ]] && { printf '%s\n' "$*" > /dev/tty; } 2>/dev/null; then
    return
  fi
  printf '%s\n' "$*" >&2
}

case "$(uname -m)" in
  arm64|aarch64)
    target_arch="arm64"
    ;;
  x86_64|amd64)
    target_arch="amd64"
    ;;
  *)
    echo "unsupported architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

log "Preparing local built-in Docker images for ployzd dev run (arch: $target_arch)..."

manifest_value() {
  local key="$1"
  local path="$2"

  if [[ ! -f "$path" ]]; then
    return 1
  fi

  awk -F'"' -v key="$key" '$1 ~ ("^" key " = ") { print $2; exit }' "$path"
}

image_exists() {
  local image_ref="$1"
  docker image inspect "$image_ref" >/dev/null 2>&1
}

relevant_inputs_dirty() {
  if ! command -v git >/dev/null 2>&1; then
    return 1
  fi

  [[ -n "$(
    git -C "$repo_dir" status --porcelain --untracked-files=all -- \
      Dockerfile.networking \
      Dockerfile.dns \
      Dockerfile.gateway \
      crates/ployz-dns \
      crates/ployz-gateway \
      crates/ployz-bpfctl \
      crates/ebpf-common \
      ebpf \
      2>/dev/null
  )" ]]
}

build_image() {
  local dockerfile="$1"
  local image_ref="$2"

  log "Building $image_ref from $dockerfile ..."
  docker build \
    --progress=plain \
    --build-arg TARGETARCH="$target_arch" \
    -f "$repo_dir/$dockerfile" \
    -t "$image_ref" \
    "$repo_dir"
  log "Finished $image_ref"
}

git_tag() {
  if ! command -v git >/dev/null 2>&1; then
    printf 'dev'
    return
  fi

  local short_ref
  short_ref="$(git -C "$repo_dir" rev-parse --short HEAD 2>/dev/null || true)"
  if [[ -z "$short_ref" ]]; then
    short_ref='dev'
  fi

  printf '%s' "$short_ref"
}

tag_suffix="$(git_tag)"
networking_ref="ployz-dev/ployz-networking:${tag_suffix}"
dns_ref="ployz-dev/ployz-dns:${tag_suffix}"
gateway_ref="ployz-dev/ployz-gateway:${tag_suffix}"

existing_networking_ref="$(manifest_value networking "$output_path" || true)"
existing_dns_ref="$(manifest_value dns "$output_path" || true)"
existing_gateway_ref="$(manifest_value gateway "$output_path" || true)"

if relevant_inputs_dirty; then
  log "Warning: built-in image inputs have local changes; cached commit-tagged images may be stale."
fi

if [[ "$existing_networking_ref" == "$networking_ref" ]] \
  && [[ "$existing_dns_ref" == "$dns_ref" ]] \
  && [[ "$existing_gateway_ref" == "$gateway_ref" ]] \
  && image_exists "$networking_ref" \
  && image_exists "$dns_ref" \
  && image_exists "$gateway_ref"; then
  log "Built-in images unchanged; skipping Docker rebuild."
  printf '%s\n' "$output_path"
  exit 0
fi

build_image "Dockerfile.networking" "$networking_ref" >&2
build_image "Dockerfile.dns" "$dns_ref" >&2
build_image "Dockerfile.gateway" "$gateway_ref" >&2

bash "$repo_dir/scripts/write-built-in-images-manifest.sh" \
  "$output_path" \
  "networking=$networking_ref" \
  "dns=$dns_ref" \
  "gateway=$gateway_ref"

log "Wrote built-in image manifest to $output_path"
