use std::process::{Command, Output};

fn relay(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz-relay"))
        .args(args)
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
fn missing_dial_credential_fails_closed() {
    let output = relay(&[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("credential must be a non-empty bearer"),
        "{}",
        stderr(&output)
    );
}
