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
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let converts = line.contains("#[serde(")
            && (line.contains("try_from = \"")
                || line.contains("into = \"")
                || line.contains(" from = \""));
        if !converts {
            continue;
        }
        let (before, from_here) = lines.split_at(index);
        let attributes: Vec<&str> = before
            .iter()
            .rev()
            .take_while(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with('#') || trimmed.starts_with("///")
            })
            .chain(
                from_here
                    .iter()
                    .take_while(|line| line.trim_start().starts_with('#')),
            )
            .copied()
            .collect();
        let derives_ts = attributes
            .iter()
            .any(|line| line.contains("derive(") && line.contains("TS"));
        let declared = attributes.iter().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("#[ts(as") || trimmed.starts_with("#[ts(type")
        });
        let item = from_here
            .iter()
            .find(|line| !line.trim_start().starts_with('#'))
            .map(|line| line.trim())
            .unwrap_or_default();
        let newtype = item.contains("struct ") && item.contains('(');
        if derives_ts && !declared && !newtype {
            offenders.push(format!("{}:{}: {item}", path.display(), index + 1));
        }
    }
}
