use std::collections::BTreeSet;
use std::path::Path;

use super::{TOOLS, ToolId};

#[test]
fn a_tool_id_names_its_tool() {
    assert_eq!(ToolId::ALL.len(), TOOLS.len());
    for (id, entry) in ToolId::ALL.iter().zip(TOOLS) {
        assert_eq!(id.name(), entry.name, "{id:?} is not in catalog order");
    }
    let names = ToolId::ALL
        .iter()
        .map(|id| id.name())
        .collect::<BTreeSet<_>>();
    assert_eq!(names.len(), ToolId::ALL.len(), "two tools share a name");
}

#[test]
fn every_tool_name_literal_in_tools_names_a_tool() {
    let names = ToolId::ALL
        .iter()
        .map(|id| id.name())
        .collect::<BTreeSet<_>>();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tools");
    let needle = format!("{}::new(", "Call");
    let mut found = 0;
    let mut unknown = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (at, _) in text.match_indices(&needle) {
            let rest = text.get(at + needle.len()..).unwrap().trim_start();
            let Some(rest) = rest.strip_prefix('"') else {
                continue;
            };
            let name = rest.split('"').next().unwrap();
            if name.is_empty() {
                continue;
            }
            found += 1;
            if !names.contains(name) {
                unknown.push((path.file_name().unwrap().to_owned(), name.to_owned()));
            }
        }
    }
    assert!(found >= 20, "the scan found {found} literals");
    assert!(unknown.is_empty(), "no tool is named {unknown:?}");
}

#[test]
fn the_tool_def_binding_is_what_a_model_client_reads() {
    use ts_rs::TS;
    assert_eq!(
        super::ToolDef::decl(&ts_rs::Config::default()),
        "type ToolDef = { name: string, title: string, description: string, input_schema: JsonValue, output_schema: JsonValue, read_only: boolean, destructive: boolean, idempotent: boolean, open_world: boolean, gate: Gate, };"
    );
}

#[test]
fn every_tool_names_a_read_to_check_what_it_started() {
    for entry in TOOLS {
        assert!(
            entry.facts.status.read_only(),
            "{} names {:?}, which writes",
            entry.name,
            entry.facts.status
        );
    }
}

#[test]
fn only_these_tools_word_an_approval_of_a_diff_otherwise() {
    use super::DiffVerb;
    use serde_json::json;

    for entry in TOOLS {
        let expected = match entry.name {
            "environment_remove" | "project_remove" => DiffVerb::Remove,
            "deployment_retry" => DiffVerb::Retry,
            _ => DiffVerb::Deploy,
        };
        assert_eq!(entry.facts.verb(&json!({})), expected, "{}", entry.name);
        let skipping = entry.facts.verb(&json!({ "skip_deploy": true }));
        let expected = match entry.name {
            "deploy" => DiffVerb::Publish,
            _ => expected,
        };
        assert_eq!(skipping, expected, "{}", entry.name);
    }
}
