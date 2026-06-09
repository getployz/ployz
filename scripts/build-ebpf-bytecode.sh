#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLCHAIN="${PLOYZ_EBPF_TOOLCHAIN:-nightly}"
TARGET_DIR="${PLOYZ_EBPF_TARGET_DIR:-${CARGO_TARGET_DIR:-/tmp/ployz-rust-ebpf-target}}"

rustup install "${TOOLCHAIN}"
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
