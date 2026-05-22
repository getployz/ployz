#!/usr/bin/env bash
set -euo pipefail

RAW_IMPORT_PATTERN='\b(use +)?(polis|p2panda|iroh)(::|\s+as\s+|\s*\{)|\bextern +crate +(polis|p2panda|iroh)\b|::mvp_'
RAW_SUBSTRATE_PATTERN='\b(RawRecord|AuthorizedRecord|ProjectionInput|RecordSource|ProofMetadata|CandidateStatus|candidate_status|VerifiedFact|FactReducer|MemoryFactStore|MemoryProjectionSource|Fact(Append|Candidate|Conflict|Cursor|Grant|Id|Key|Kind|Payload|Query|Receipt|Rejection|Replay|Store|Target|Write)[A-Za-z0-9_]*|Projection(CatchUp|Error|Freshness|Health|Key|Request|Snapshot|Source|View)[A-Za-z0-9_]*)\b'
RAW_ATTEMPT_PATTERN='\b(external_attempt|CommandRunner|Attempt(Log|Backend(Request|Start)?|Evidence(Kind)?|Failure(Codec|DecodeError)|Fingerprint(Builder)?|Kind|Replay|Request|Resource|Start|TerminalMarker)|TypedAttempt[A-Za-z0-9_]*|IssuedProductAttempt|IssuedDeployCommand|IssuedVolumeTransferCommand|OpenAttempt|MutationIntent|CommandPayload|record_evidence|terminalize|begin_attempt|begin_typed_attempt)\b'
POLIS_ROOT_OPERATION_PATTERN='(^pub[[:space:]]+mod[[:space:]]+operations;|^pub[[:space:]]+use[[:space:]]+external_attempt|\b(CommandRunner|Attempt(Log|Backend(Request|Start)?|Evidence(Kind)?|Failure(Codec|DecodeError)|Fingerprint(Builder)?|Kind|Replay|Request|Resource|Start|TerminalMarker)|TypedAttempt[A-Za-z0-9_]*|OpenOperation|OperationReplay|OperationStart|start_or_replay|begin_attempt|begin_typed_attempt|AttemptStart|OpenAttempt|AttemptTerminal)\b)'

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

scan_polis_root_exports() {
  local root="$1"
  grep -nE "$POLIS_ROOT_OPERATION_PATTERN" "$root/crates/polis/src/lib.rs"
}

scan_ployz_attempt_usage() {
  local root="$1"
  local matched=1

  while IFS= read -r -d '' file; do
    case "$file" in
      crates/ployz/src/adapters/polis/acme.rs|crates/ployz/src/composition.rs)
        continue
        ;;
    esac

    if (cd "$root" && grep -nE "$RAW_ATTEMPT_PATTERN" "$file"); then
      matched=0
    fi
  done < <((cd "$root" && find crates/ployz/src -type f -name '*.rs' -print0))

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

  reset_polis_fixture() {
    rm -rf "$tmp/crates"
    mkdir -p "$tmp/crates/polis/src"
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
  printf 'pub struct Bad(AttemptFailureCodec);\n' > "$tmp/crates/ployz/src/deploy/mod.rs"
  if ! scan_feature_files "$tmp" "$RAW_ATTEMPT_PATTERN" >/dev/null; then
    echo "boundary self-test failed to catch raw attempt failure codec" >&2
    exit 1
  fi

  reset_polis_fixture
  printf 'pub mod operations;\n' > "$tmp/crates/polis/src/lib.rs"
  if ! scan_polis_root_exports "$tmp" >/dev/null; then
    echo "boundary self-test failed to catch public polis operations module" >&2
    exit 1
  fi

  reset_polis_fixture
  printf 'mod operations;\npub use operations::{TypedAttemptRequest};\n' \
    > "$tmp/crates/polis/src/lib.rs"
  if ! scan_polis_root_exports "$tmp" >/dev/null; then
    echo "boundary self-test failed to catch typed attempt root export" >&2
    exit 1
  fi

  reset_polis_fixture
  printf 'mod operations;\npub mod external_attempt;\npub use operations::{OperationId};\n' \
    > "$tmp/crates/polis/src/lib.rs"
  if scan_polis_root_exports "$tmp" >/dev/null; then
    echo "boundary self-test rejected external attempt module with shared identity export" >&2
    exit 1
  fi

  reset_polis_fixture
  printf 'mod operations;\npub mod external_attempt;\npub use external_attempt::{Backend, Request};\n' \
    > "$tmp/crates/polis/src/lib.rs"
  if ! scan_polis_root_exports "$tmp" >/dev/null; then
    echo "boundary self-test failed to catch external attempt root re-export" >&2
    exit 1
  fi

  reset_polis_fixture
  printf 'mod operations;\npub mod external_attempt;\npub use external_attempt::*;\n' \
    > "$tmp/crates/polis/src/lib.rs"
  if ! scan_polis_root_exports "$tmp" >/dev/null; then
    echo "boundary self-test failed to catch external attempt wildcard root re-export" >&2
    exit 1
  fi

  reset_fixture
  mkdir -p "$tmp/crates/ployz/src/adapters/polis"
  printf 'use polis::external_attempt as attempt;\n' \
    > "$tmp/crates/ployz/src/adapters/polis/machine.rs"
  if ! scan_ployz_attempt_usage "$tmp" >/dev/null; then
    echo "boundary self-test failed to catch non-ACME attempt adapter" >&2
    exit 1
  fi

  reset_fixture
  mkdir -p "$tmp/crates/ployz/src/adapters/polis"
  printf 'use polis::external_attempt as attempt;\n' \
    > "$tmp/crates/ployz/src/adapters/polis/acme.rs"
  if scan_ployz_attempt_usage "$tmp" >/dev/null; then
    echo "boundary self-test rejected ACME attempt adapter" >&2
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

if scan_polis_root_exports "."; then
  echo "polis root exports must not expose generic or typed attempt APIs directly" >&2
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

if scan_ployz_attempt_usage "."; then
  echo "polis external attempts are allowed only in ACME adapter and composition wiring" >&2
  exit 1
fi
