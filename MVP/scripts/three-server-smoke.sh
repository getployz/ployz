#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "${ROOT_DIR}"
cargo build -p mvp-node -p mvp-e2e
MVP_NODE_BIN="${ROOT_DIR}/target/debug/mvp-node" \
  cargo run -p mvp-e2e -- three-server-product
