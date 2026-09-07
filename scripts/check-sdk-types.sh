#!/usr/bin/env bash
# Type-check the @ployz/sdk package against the derived declarations.
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT/ployz-sdk"
[ -d node_modules/typescript ] || npm ci --ignore-scripts
npx --no-install tsc --noEmit
