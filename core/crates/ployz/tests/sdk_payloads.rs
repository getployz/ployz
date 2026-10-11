//! `ployz-sdk/generated/payloads.d.ts` is derived from the Rust wire types.
//! Regenerate with `PLOYZ_WRITE_SDK_PAYLOADS=1 cargo test -p ployz --test sdk_payloads`.

use std::{env, fs, path::PathBuf};

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

/// `ployz-sdk/generated/catalog.json` is the settings catalog the dashboard takes its field
/// labels and help from. Regenerate with the same `PLOYZ_WRITE_SDK_PAYLOADS=1` run.
#[test]
fn generated_catalog_matches_checked_in_file() {
    let catalog = ployz_store::catalog::schema(None).expect("the whole catalog");
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&catalog).expect("serialize catalog")
    );
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ployz-sdk/generated/catalog.json");
    if env::var_os("PLOYZ_WRITE_SDK_PAYLOADS").is_some() {
        fs::write(&path, &generated).expect("write catalog.json");
        return;
    }
    let checked_in = fs::read_to_string(&path).expect("read catalog.json");
    assert!(
        checked_in == generated,
        "{} is stale; run `PLOYZ_WRITE_SDK_PAYLOADS=1 cargo test -p ployz --test sdk_payloads`",
        path.display()
    );
}
