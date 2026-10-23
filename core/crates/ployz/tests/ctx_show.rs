use std::process::{Command, Output};

fn ctx(json: bool, token: Option<&str>) -> Output {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .args(["ctx", "--color", "never", "--ployz-config"])
        .arg(home.path().join("config.yaml"))
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PLOYZ_CONTEXT")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV")
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9");
    if json {
        command.arg("--json");
    }
    if let Some(token) = token {
        command.env("PLOYZ_TOKEN", token);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

#[test]
fn signed_out_ctx_names_no_one_and_login() {
    let human = ctx(false, None);
    let stdout = String::from_utf8(human.stdout).unwrap();
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(stdout.contains("organization = no one\n"), "{stdout}");
    assert_eq!(stderr, "next: ployz login\n");

    let json: serde_json::Value = serde_json::from_slice(&ctx(true, None).stdout).unwrap();
    assert_eq!(
        json.pointer("/organization"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(json.pointer("/credential"), Some(&serde_json::Value::Null));
    assert_eq!(
        json.pointer("/next"),
        Some(&serde_json::json!("ployz login"))
    );
}

#[test]
fn ctx_reports_whether_the_local_fallback_socket_exists() {
    let exists = std::path::Path::new(ployz::connect::DEFAULT_LOCAL_SOCKET).exists();
    let human = ctx(false, None);
    let stdout = String::from_utf8(human.stdout).unwrap();
    if exists {
        assert!(stdout.contains("servers = via local\n"), "{stdout}");
    } else {
        assert!(
            stdout.contains("servers = (none: no local daemon socket)\n"),
            "{stdout}"
        );
    }
    let json: serde_json::Value = serde_json::from_slice(&ctx(true, None).stdout).unwrap();
    assert_eq!(
        json.pointer("/servers/via"),
        Some(&serde_json::json!("local"))
    );
    assert_eq!(
        json.pointer("/servers/local_socket_exists"),
        Some(&serde_json::json!(exists))
    );
}

#[test]
fn token_ctx_keeps_the_unknown_organization_and_cloud_route() {
    let human = ctx(false, Some("ployz_example"));
    let stdout = String::from_utf8(human.stdout).unwrap();
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(
        stdout.contains("organization = (the token's)\n"),
        "{stdout}"
    );
    assert!(stdout.contains("servers = via cloud\n"), "{stdout}");
    assert_eq!(stderr, "next: ployz link --project PROJECT\n");
    let json: serde_json::Value =
        serde_json::from_slice(&ctx(true, Some("ployz_example")).stdout).unwrap();
    assert_eq!(
        json.pointer("/organization"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(
        json.pointer("/servers/via"),
        Some(&serde_json::json!("cloud"))
    );
}
