#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLCHAIN="${PLOYZ_EBPF_TOOLCHAIN:-nightly}"

rustup install "${TOOLCHAIN}"
rustup component add rust-src --toolchain "${TOOLCHAIN}"
if ! command -v bpf-linker >/dev/null 2>&1; then
  cargo +"${TOOLCHAIN}" install --locked bpf-linker
fi

cargo +"${TOOLCHAIN}" build \
  -Z build-std=core \
  --release \
  --target bpfel-unknown-none \
  --manifest-path "${ROOT_DIR}/ebpf/Cargo.toml"
