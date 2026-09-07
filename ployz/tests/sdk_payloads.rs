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
