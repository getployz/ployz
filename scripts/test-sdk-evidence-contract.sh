#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir "$TMP/src"
cat > "$TMP/Cargo.toml" <<'TOML'
[package]
name = "sdk-evidence-contract"
version = "0.0.0"
edition = "2024"
[features]
mismatched = []
[dependencies]
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
TOML
cat > "$TMP/src/main.rs" <<EOF_RUST
#[path = "$ROOT/ployz-sdk-payloads/src/generate/evidence.rs"]
mod evidence;
EOF_RUST
cat >> "$TMP/src/main.rs" <<'RUST'
use evidence::{RustEvidence, TaggedEvidence};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Actual { Ready }
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Other { Ready, #[serde(other)] Unknown }

#[cfg(not(feature = "mismatched"))]
use Actual as Example;
#[cfg(feature = "mismatched")]
use Other as Example;

const EVIDENCE: &dyn TaggedEvidence = &RustEvidence::<Actual>(|| vec![Example::Ready]);

fn main() {
    let ready = serde_json::to_value(Other::Ready).unwrap();
    assert_eq!(EVIDENCE.examples(), vec![ready.clone()]);
    assert!(EVIDENCE.decodes(ready));
    let unknown = serde_json::json!({"kind":"future"});
    assert!(serde_json::from_value::<Other>(unknown.clone()).is_ok());
    assert!(!EVIDENCE.decodes(unknown));
}
RUST

# A separate target avoids locking the workspace when invoked from its checks.
export CARGO_TARGET_DIR="$ROOT/target/sdk-evidence-contract"
cargo run --quiet --offline --manifest-path "$TMP/Cargo.toml"
if cargo check --quiet --offline --manifest-path "$TMP/Cargo.toml" --features mismatched > "$TMP/error.log" 2>&1; then
    echo 'SDK evidence accepted examples from another Rust type' >&2
    exit 1
fi
grep -q 'expected.*Actual.*found.*Other' "$TMP/error.log"
echo 'SDK evidence rejects structurally compatible examples from another Rust type'
