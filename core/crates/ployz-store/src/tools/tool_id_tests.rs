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
