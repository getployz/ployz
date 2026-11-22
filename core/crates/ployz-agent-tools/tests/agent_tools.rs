//! The agent tool catalog as a model sees it, checked against a snapshot.

use std::path::PathBuf;

use ployz_agent_tools::{
    DESCRIPTION, INSTRUCTIONS, Place, Step, StoreAt, TITLE, ToolResult, catalog, start,
};
use ployz_store::OrganizationId;
use serde_json::{Value, json};

const CAP_BYTES: usize = 79_000;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/agent_tools.json")
}

fn compact() -> Value {
    json!({ "title": TITLE, "description": DESCRIPTION, "instructions": INSTRUCTIONS, "tools": catalog() })
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

// The cap counts what a model reads every turn. Hosts show titles and hints to
// people and check output against its schema; none of them reaches the model.
#[test]
fn the_catalog_fits_the_cap() {
    let mut tools = serde_json::to_value(catalog()).unwrap();
    for tool in tools.as_array_mut().unwrap() {
        let tool = tool.as_object_mut().unwrap();
        for unread in ["output_schema", "title", "idempotent", "open_world"] {
            tool.remove(unread);
        }
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
    "first.",
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

// A description may name one other tool, to say what to use instead or what
// comes next; a field never does. Workflow belongs in INSTRUCTIONS.
// Single-word names (docs, review, discard, history, deploy) are the domain's own nouns, as in "the next deploy".
#[test]
fn descriptions_say_what_a_tool_does_and_name_at_most_one_other() {
    let tools = catalog();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool.name)
        .filter(|name| name.contains('_'))
        .collect();
    let mut offences = Vec::new();
    for tool in &tools {
        let routed: Vec<&&str> = names
            .iter()
            .filter(|name| **name != tool.name && names_whole_word(&tool.description, name))
            .collect();
        if routed.len() > 1 {
            offences.push(format!("{}: names {routed:?}", tool.name));
        }
        let lower = tool.description.to_lowercase();
        for phrase in IMPERATIVES
            .iter()
            .filter(|phrase| names_whole_word(&lower, phrase))
        {
            offences.push(format!("{}: says {phrase:?}", tool.name));
        }
        let mut texts = Vec::new();
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

// Every input struct is `deny_unknown_fields`, written by hand or by `tool_input!`.
#[test]
fn every_tool_refuses_an_unknown_field_and_names_the_fields_it_has() {
    let place = Place {
        project: None,
        environment: None,
        store: StoreAt::Cloud {
            organization: OrganizationId::parse("org").unwrap(),
        },
    };
    for tool in catalog() {
        let (step, _) = start(
            tool.name,
            json!({ "unknown_probe": 1 }),
            &place,
            "probe-0001",
        );
        let Step::Done {
            result: ToolResult::Refused(refusal),
        } = step
        else {
            panic!("{} took an unknown field", tool.name);
        };
        let mut named: Vec<&str> = refusal.message.split('`').skip(1).step_by(2).collect();
        named.sort_unstable();
        let mut declared: Vec<&str> = tool
            .input_schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().map(String::as_str).collect())
            .unwrap_or_default();
        declared.sort_unstable();
        assert!(
            refusal.message.contains("unknown field"),
            "{}: {}",
            tool.name,
            refusal.message
        );
        assert_eq!(named, declared, "{}: {}", tool.name, refusal.message);
    }
}

// Claude Code cuts server instructions past 2 KB; `ployz mcp` adds a sentence.
#[test]
fn the_instructions_fit_what_hosts_keep() {
    let bytes = INSTRUCTIONS.len();
    assert!(
        bytes <= 1_900,
        "the instructions are {bytes} B, over 1900 B"
    );
}

#[test]
fn every_tool_has_a_short_title() {
    let offences: Vec<String> = catalog()
        .iter()
        .filter(|tool| {
            let title = tool.title;
            title.is_empty()
                || title.len() > 40
                || title.ends_with('.')
                || !title.starts_with(|c: char| c.is_ascii_uppercase())
        })
        .map(|tool| format!("{}: {:?}", tool.name, tool.title))
        .collect();
    assert!(offences.is_empty(), "{}", offences.join("\n"));
}

fn undescribed(schema: &Value, path: &str, found: &mut Vec<String>) {
    let Value::Object(fields) = schema else {
        return;
    };
    for list in ["properties", "patternProperties"] {
        for (name, property) in fields
            .get(list)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let here = format!("{path}/{name}");
            if property.get("description").is_none() {
                found.push(here.clone());
            }
            undescribed(property, &here, found);
        }
    }
    if let Some(items) = fields.get("items") {
        undescribed(items, path, found);
    }
    for option in fields
        .get("anyOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        undescribed(option, path, found);
    }
}

#[test]
fn every_input_field_says_what_it_means() {
    let mut found = Vec::new();
    for tool in catalog() {
        undescribed(&tool.input_schema, tool.name, &mut found);
    }
    assert!(
        found.is_empty(),
        "fields without a description:\n{}",
        found.join("\n")
    );
}
