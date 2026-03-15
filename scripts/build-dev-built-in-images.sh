#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_path="${1:-$repo_dir/target/ployz-dev/built-in-images.toml}"

log() {
  if [[ -w /dev/tty ]]; then
    printf '%s\n' "$*" > /dev/tty
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

  local short_ref dirty_count
  short_ref="$(git -C "$repo_dir" rev-parse --short HEAD 2>/dev/null || true)"
  if [[ -z "$short_ref" ]]; then
    short_ref='dev'
  fi

  dirty_count="$(
    git -C "$repo_dir" status --porcelain --untracked-files=all -- \
      Dockerfile.networking \
      Dockerfile.dns \
      Dockerfile.gateway \
      crates/ployz-dns \
      crates/ployz-gateway \
      crates/ployz-bpfctl \
      crates/ebpf-common \
      ebpf \
      2>/dev/null | wc -l | tr -d '[:space:]'
  )"

  if [[ "$dirty_count" == "0" ]]; then
    printf '%s' "$short_ref"
    return
  fi

  printf '%s-dirty-%s' "$short_ref" "$(date +%s)"
}

tag_suffix="$(git_tag)"
networking_ref="ployz-dev/ployz-networking:${tag_suffix}"
dns_ref="ployz-dev/ployz-dns:${tag_suffix}"
gateway_ref="ployz-dev/ployz-gateway:${tag_suffix}"

build_image "Dockerfile.networking" "$networking_ref" >&2
build_image "Dockerfile.dns" "$dns_ref" >&2
build_image "Dockerfile.gateway" "$gateway_ref" >&2

bash "$repo_dir/scripts/write-built-in-images-manifest.sh" \
  "$output_path" \
  "networking=$networking_ref" \
  "dns=$dns_ref" \
  "gateway=$gateway_ref"

log "Wrote built-in image manifest to $output_path"
