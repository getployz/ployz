#!/usr/bin/env bash
set -euo pipefail

metadata="$(cargo metadata --format-version 1 --no-deps)"

python3 - "$metadata" <<'PY'
import json
import sys

metadata = json.loads(sys.argv[1])
bad = []
for package in metadata["packages"]:
    manifest = package["manifest_path"]
    if "/legacy/" in manifest or "/tailscale-rs/" in manifest:
        bad.append(manifest)

if bad:
    print("root workspace includes forbidden package manifests:", file=sys.stderr)
    for manifest in bad:
        print(f"  {manifest}", file=sys.stderr)
    sys.exit(1)
PY

if grep -R --include='*.rs' -nE '\b(use +)?ployz(::|\s+as\s+|\s*::|\s*\{)|\bextern +crate +ployz\b' crates/polis; then
  echo "polis must not import ployz" >&2
  exit 1
fi

if find crates/ployz/src -type f -name '*.rs' \
  ! -path 'crates/ployz/src/adapters/*' \
  ! -path 'crates/ployz/src/composition.rs' \
  -print0 | xargs -0 grep -nE '\b(use +)?(polis|p2panda|iroh)(::|\s+as\s+|\s*\{)|\bextern +crate +(polis|p2panda|iroh)\b|::mvp_' ; then
  echo "ployz feature modules must not import polis, p2panda, iroh, or legacy mvp symbols" >&2
  exit 1
fi

if find crates/ployz/src -type f -name '*.rs' \
  ! -path 'crates/ployz/src/adapters/*' \
  ! -path 'crates/ployz/src/composition.rs' \
  -print0 | xargs -0 grep -nE '\b(RawRecord|AuthorizedRecord|ProjectionInput|RecordSource|ProofMetadata|CandidateStatus|candidate_status)\b' ; then
  echo "ployz feature modules must not mention raw substrate record/projection internals" >&2
  exit 1
fi
