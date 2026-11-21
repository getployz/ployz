#!/usr/bin/env bash
# Exits 0 when the dashboard's native SDK (crates/ployz-sdk/ployz-sdk.node) is missing or older than a core Rust source
# or Cargo.toml, so scripts/build-cloud-sdk.sh has work to do; exits 1 when it is current. Shared by
# dashboard/scripts/verify/up.sh and `verify-cluster update`.
set -euo pipefail
crates=$(CDPATH='' cd -- "$(dirname -- "$0")/../crates" && pwd)
sdk=$crates/ployz-sdk/ployz-sdk.node
[ ! -f "$sdk" ] || [ -n "$(find "$crates" -newer "$sdk" -type f \( -name '*.rs' -o -name Cargo.toml \) -print -quit)" ]
