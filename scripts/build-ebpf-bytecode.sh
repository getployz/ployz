#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLCHAIN="${PLOYZ_EBPF_TOOLCHAIN:-nightly}"
TARGET_DIR="${PLOYZ_EBPF_TARGET_DIR:-${CARGO_TARGET_DIR:-/tmp/ployz-rust-ebpf-target}}"

# `rustup install nightly` on a machine that already has a nightly syncs the
# channel to the latest snapshot; inside a baked builder image that update
# fails on overlayfs (EXDEV renaming directories from an image layer), so an
# already-installed toolchain is used as-is.
if ! rustup toolchain list | grep -q "^${TOOLCHAIN}"; then
  rustup install "${TOOLCHAIN}"
fi
rustup component add rust-src --toolchain "${TOOLCHAIN}"
if ! command -v bpf-linker >/dev/null 2>&1; then
  cargo +"${TOOLCHAIN}" install --locked bpf-linker
fi

cargo +"${TOOLCHAIN}" build \
  -Z build-std=core \
  --release \
  --target bpfel-unknown-none \
  --target-dir "${TARGET_DIR}" \
  --manifest-path "${ROOT_DIR}/ebpf/Cargo.toml"

echo "${TARGET_DIR}/bpfel-unknown-none/release/ployz-ebpf-tc"
