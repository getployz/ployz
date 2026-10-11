//! The agent tool catalog as a model sees it, checked against a snapshot.

use std::path::PathBuf;

use ployz_store::tools::{INSTRUCTIONS, catalog};
use serde_json::{Value, json};

const CAP_BYTES: usize = 48_000;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/agent_tools.json")
}

fn compact() -> Value {
    json!({ "instructions": INSTRUCTIONS, "tools": catalog() })
}

#[test]
fn the_catalog_matches_its_snapshot() {
    let mut text = serde_json::to_string_pretty(&compact()).unwrap();
    text.push('\n');
    let path = snapshot_path();
    if std::env::var_os("PLOYZ_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
    }
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        saved == text,
        "the catalog changed; rerun with PLOYZ_BLESS=1 and review tests/snapshots/agent_tools.json"
    );
}

// The cap counts what a model reads every turn, so output schemas are left out.
#[test]
fn the_catalog_fits_the_cap() {
    let mut sent = compact();
    for tool in sent["tools"].as_array_mut().unwrap() {
        tool.as_object_mut().unwrap().remove("output_schema");
    }
    let bytes = serde_json::to_vec(&sent).unwrap().len();
    println!("catalog bytes: {bytes}");
    assert!(
        bytes < CAP_BYTES,
        "the catalog is {bytes} B, over {CAP_BYTES} B"
    );
}
