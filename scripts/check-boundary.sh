#!/usr/bin/env bash
set -euo pipefail

RAW_IMPORT_PATTERN='\b(use +)?(polis|p2panda|iroh)(::|\s+as\s+|\s*\{)|\bextern +crate +(polis|p2panda|iroh)\b|::mvp_'
RAW_SUBSTRATE_PATTERN='\b(RawRecord|AuthorizedRecord|ProjectionInput|RecordSource|ProofMetadata|CandidateStatus|candidate_status|VerifiedFact|FactReducer|MemoryFactStore|MemoryProjectionSource|Fact(Append|Candidate|Conflict|Cursor|Grant|Id|Key|Kind|Payload|Query|Receipt|Rejection|Replay|Store|Target|Write)[A-Za-z0-9_]*|Projection(CatchUp|Error|Freshness|Health|Key|Request|Snapshot|Source|View)[A-Za-z0-9_]*)\b'
RAW_ATTEMPT_PATTERN='\b(CommandRunner|AttemptLog|AttemptBackend|AttemptReplay|AttemptRequest|AttemptStart|IssuedProductAttempt|IssuedDeployCommand|IssuedVolumeTransferCommand|OpenAttempt|TerminalMarker|MutationIntent|CommandPayload|CommandKind|FingerprintedResource|record_evidence|terminalize|begin_attempt|begin_typed_attempt)\b'

feature_files() {
  find crates/ployz/src -type f -name '*.rs' \
    ! -path 'crates/ployz/src/adapters/*' \
    ! -path 'crates/ployz/src/composition.rs' \
    ! -path 'crates/ployz/src/operation.rs' \
    ! -path 'crates/ployz/src/operation/*' \
    -print0
}

scan_feature_files() {
  local root="$1"
  local pattern="$2"
  local matched=1

  while IFS= read -r -d '' file; do
    if (cd "$root" && grep -nE "$pattern" "$file"); then
      matched=0
    fi
  done < <((cd "$root" && feature_files))

  return "$matched"
}

run_self_test() {
  local tmp
  tmp="$(mktemp -d)"
  trap "rm -rf '$tmp'" EXIT

  reset_fixture() {
    rm -rf "$tmp/crates"
    mkdir -p "$tmp/crates/ployz/src/deploy" "$tmp/crates/ployz/src/facts"
  }

  reset_fixture
  printf 'pub type Bad = FactCandidate;\n' > "$tmp/crates/ployz/src/deploy/mod.rs"
  if ! scan_feature_files "$tmp" "$RAW_SUBSTRATE_PATTERN" >/dev/null; then
    echo "boundary self-test failed to catch raw type alias" >&2
    exit 1
  fi

  reset_fixture
  printf 'pub use polis::ProjectionSnapshot;\n' > "$tmp/crates/ployz/src/deploy/mod.rs"
  if ! scan_feature_files "$tmp" "$RAW_IMPORT_PATTERN" >/dev/null; then
    echo "boundary self-test failed to catch raw re-export" >&2
    exit 1
  fi

  reset_fixture
  printf 'pub use polis::FactCandidate;\n' > "$tmp/crates/ployz/src/facts/mod.rs"
  if ! scan_feature_files "$tmp" "$RAW_IMPORT_PATTERN" >/dev/null; then
    echo "boundary self-test failed to protect public facts module" >&2
    exit 1
  fi

  reset_fixture
  printf 'pub struct Bad(AttemptRequest);\n' > "$tmp/crates/ployz/src/deploy/mod.rs"
  if ! scan_feature_files "$tmp" "$RAW_ATTEMPT_PATTERN" >/dev/null; then
    echo "boundary self-test failed to catch raw attempt orchestration" >&2
    exit 1
  fi

  reset_fixture
  printf 'use crate::facts::ProductFactCursor;\npub struct Good(ProductFactCursor);\n' \
    > "$tmp/crates/ployz/src/deploy/mod.rs"
  if scan_feature_files "$tmp" "$RAW_IMPORT_PATTERN" >/dev/null \
    || scan_feature_files "$tmp" "$RAW_SUBSTRATE_PATTERN" >/dev/null \
    || scan_feature_files "$tmp" "$RAW_ATTEMPT_PATTERN" >/dev/null; then
    echo "boundary self-test rejected product-owned fact vocabulary" >&2
    exit 1
  fi
}

if [[ "${1:-}" == "--self-test" ]]; then
  run_self_test
  exit 0
fi

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

if scan_feature_files "." "$RAW_IMPORT_PATTERN"; then
  echo "ployz feature modules must not import polis, p2panda, iroh, or legacy mvp symbols" >&2
  exit 1
fi

if scan_feature_files "." "$RAW_SUBSTRATE_PATTERN"; then
  echo "ployz feature modules must not mention raw substrate record/projection internals" >&2
  exit 1
fi

if scan_feature_files "." "$RAW_ATTEMPT_PATTERN"; then
  echo "ployz feature modules must not mention generic attempt or command-runner orchestration" >&2
  exit 1
fi
