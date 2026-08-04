#!/usr/bin/env bash
set -euo pipefail

# Compile and test the role-neutral DinD harness. Product slices add public-seam
# scenarios here when they need real Docker or multi-process proof.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

cargo test -p ployz-e2e --lib "$@"
