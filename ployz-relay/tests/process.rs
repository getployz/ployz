use std::process::{Command, Output};

fn relay(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz-relay"))
        .args(args)
        .env_remove("PLOYZ_RELAY_PAIRING_CREDENTIAL")
        .env_remove("PLOYZ_RELAY_DIAL_CREDENTIAL")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_prints_the_package_version() {
    let output = relay(&["version"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        output.stdout,
        format!("{}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    );
}

#[test]
fn missing_credentials_fail_closed() {
    let output = relay(&[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("credential must be a non-empty bearer"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn identical_credentials_fail_closed() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployz-relay"))
        .env("PLOYZ_RELAY_PAIRING_CREDENTIAL", "same")
        .env("PLOYZ_RELAY_DIAL_CREDENTIAL", "same")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("Pairing Credential and Dial Credential must be distinct"),
        "{}",
        stderr(&output)
    );
}
