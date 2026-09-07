//! `ployz-sdk/generated/payloads.d.ts` is derived from the Rust wire types.
//! Regenerate with `PLOYZ_WRITE_SDK_PAYLOADS=1 cargo test -p ployz --test sdk_payloads`.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[test]
fn generated_declarations_match_checked_in_file() {
    let generated = ployz::sdk::typescript_declarations();
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ployz-sdk/generated/payloads.d.ts");
    if env::var_os("PLOYZ_WRITE_SDK_PAYLOADS").is_some() {
        fs::write(&path, &generated).expect("write payloads.d.ts");
        return;
    }
    let checked_in = fs::read_to_string(&path).expect("read payloads.d.ts");
    assert!(
        checked_in == generated,
        "{} is stale; run `PLOYZ_WRITE_SDK_PAYLOADS=1 cargo test -p ployz --test sdk_payloads`",
        path.display()
    );
}

/// ts-rs reads serde's renames and tags but not `try_from`/`into`, so a type
/// that converts through another must say what it looks like in TypeScript.
/// Newtypes are exempt: their inner type is the wire type.
#[test]
fn converting_types_declare_their_typescript_shape() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut offenders = Vec::new();
    for crate_dir in ["ployz-core/src", "ployz/src"] {
        visit(&root.join(crate_dir), &mut offenders);
    }
    assert!(
        offenders.is_empty(),
        "serde-converting types without `#[ts(as = ...)]` or `#[ts(type = ...)]`:\n{}",
        offenders.join("\n")
    );
}

fn visit(dir: &Path, offenders: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read source directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            visit(&path, offenders);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            check_file(&path, offenders);
        }
    }
}

fn check_file(path: &Path, offenders: &mut Vec<String>) {
    let source = fs::read_to_string(path).expect("read source file");
    // Attributes and doc comments preceding an item, joined so a `#[serde(...)]`
    // that rustfmt spread over several lines reads as one.
    let mut attributes = String::new();
    let mut first_line = 0;
    let mut open_brackets = 0;
    let mut close_brackets = 0;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        let continues = open_brackets > close_brackets;
        if continues || trimmed.starts_with('#') || trimmed.starts_with("//") {
            if attributes.is_empty() {
                first_line = index + 1;
            }
            attributes.push_str(trimmed);
            attributes.push(' ');
            if continues || trimmed.starts_with('#') {
                open_brackets += trimmed.matches('[').count();
                close_brackets += trimmed.matches(']').count();
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let converts = attributes.contains("try_from = \"")
            || attributes.contains("into = \"")
            || attributes.contains("(from = \"")
            || attributes.contains(" from = \"");
        let derives_ts = attributes
            .split("derive(")
            .skip(1)
            .filter_map(|rest| rest.split(')').next())
            .any(|list| list.split(',').any(|name| name.trim() == "TS"));
        let declared = attributes.contains("#[ts(as") || attributes.contains("#[ts(type");
        let newtype = trimmed.contains("struct ") && trimmed.contains('(');
        if converts && derives_ts && !declared && !newtype {
            offenders.push(format!("{}:{first_line}: {trimmed}", path.display()));
        }
        attributes.clear();
        open_brackets = 0;
        close_brackets = 0;
    }
}
