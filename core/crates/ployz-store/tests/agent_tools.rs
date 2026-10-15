//! The agent tool catalog as a model sees it, checked against a snapshot.

use std::path::PathBuf;

use ployz_store::tools::{INSTRUCTIONS, catalog};
use serde_json::{Value, json};

const CAP_BYTES: usize = 57_150;

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
    let mut tools = serde_json::to_value(catalog()).unwrap();
    for tool in tools.as_array_mut().unwrap() {
        tool.as_object_mut().unwrap().remove("output_schema");
    }
    let sent = json!({ "instructions": INSTRUCTIONS, "tools": tools });
    let bytes = serde_json::to_vec(&sent).unwrap().len();
    println!("catalog bytes: {bytes}");
    assert!(
        bytes < CAP_BYTES,
        "the catalog is {bytes} B, over {CAP_BYTES} B"
    );
}

const IMPERATIVES: &[&str] = &[
    "do not",
    "don't",
    "never follow",
    "follow it",
    "call it",
    "use it",
    "alongside",
    " first.",
    "before changing",
    "ask the member",
    "instructions",
    "when the member asked",
    "omit unless",
    "deploy next",
    "repeat the same",
    "only from a refusal",
];

fn descriptions<'a>(schema: &'a Value, found: &mut Vec<&'a str>) {
    match schema {
        Value::Object(fields) => {
            if let Some(Value::String(text)) = fields.get("description") {
                found.push(text);
            }
            fields.values().for_each(|value| descriptions(value, found));
        }
        Value::Array(items) => items.iter().for_each(|item| descriptions(item, found)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn names_whole_word(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

// Single-word names (docs, review, discard, history, deploy) are the domain's own nouns, as in "the next deploy".
#[test]
fn descriptions_say_what_a_tool_does_and_never_route_to_another() {
    let tools = catalog();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool.name)
        .filter(|name| name.contains('_'))
        .collect();
    let mut offences = Vec::new();
    for tool in &tools {
        let mut texts = vec![tool.description.as_str()];
        descriptions(&tool.input_schema, &mut texts);
        descriptions(&tool.output_schema, &mut texts);
        for text in texts {
            let lower = text.to_lowercase();
            for other in names.iter().filter(|name| **name != tool.name) {
                if names_whole_word(text, other) {
                    offences.push(format!("{}: names {other}: {text}", tool.name));
                }
            }
            for phrase in IMPERATIVES
                .iter()
                .filter(|phrase| names_whole_word(&lower, phrase))
            {
                offences.push(format!("{}: says {phrase:?}: {text}", tool.name));
            }
        }
    }
    assert!(offences.is_empty(), "{}", offences.join("\n"));
}
