use std::process::{Command, Output};

fn ctx(json: bool, token: Option<&str>) -> Output {
    ctx_with(json, token, &[], None, None, &[], false)
}

fn ctx_with(
    json: bool,
    token: Option<&str>,
    args: &[&str],
    config: Option<&str>,
    cloud: Option<&str>,
    environment: &[(&str, &str)],
    linked_project: bool,
) -> Output {
    let home = tempfile::tempdir().unwrap();
    if let Some(config) = config {
        std::fs::write(home.path().join("config.yaml"), config).unwrap();
    }
    if let Some(cloud) = cloud {
        std::fs::write(home.path().join("cloud.json"), cloud).unwrap();
    }
    if linked_project {
        let links = std::collections::BTreeMap::from([(
            home.path().to_str().unwrap().to_owned(),
            serde_json::json!({"project": "shop", "environment": "production"}),
        )]);
        std::fs::write(
            home.path().join("links.json"),
            serde_json::to_vec(&links).unwrap(),
        )
        .unwrap();
    }
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
    command.envs(environment.iter().copied());
    command.args(args);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

#[test]
fn signed_out_ctx_names_no_sign_in_and_login() {
    // The default daemon socket can't be redirected, so a host running ployzd
    // has a local route and this signed-out-with-no-route case can't occur.
    if std::path::Path::new("/run/ployz/ployz.sock")
        .try_exists()
        .unwrap_or(true)
    {
        return;
    }
    let human = ctx(false, None);
    let stdout = String::from_utf8(human.stdout).unwrap();
    let stderr = String::from_utf8(human.stderr).unwrap();
    assert!(
        stdout.contains("organization = (none: not signed in)\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("servers = (none: no local daemon socket)\n"),
        "{stdout}"
    );
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
        json.pointer("/credential"),
        Some(&serde_json::json!("token"))
    );
    assert_eq!(
        json.pointer("/servers/via"),
        Some(&serde_json::json!("cloud"))
    );
}

const STANDALONE_CONTEXT: &str = "current_context: lab\ncontexts:\n  lab:\n    connections:\n      - tcp: 127.0.0.1:1\n        machine_id: 0123456789abcdef0123456789abcdef\n        name: machine-1\n";

#[test]
fn signed_out_with_a_project_and_standalone_context_has_no_hint() {
    let human = ctx_with(false, None, &[], Some(STANDALONE_CONTEXT), None, &[], true);
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("project = shop (link)\n"), "{stdout}");
    assert!(stdout.contains("servers = via context lab\n"), "{stdout}");
    assert_eq!(String::from_utf8(human.stderr).unwrap(), "");

    let json: serde_json::Value = serde_json::from_slice(
        &ctx_with(true, None, &[], Some(STANDALONE_CONTEXT), None, &[], true).stdout,
    )
    .unwrap();
    assert_eq!(
        json.pointer("/servers/via"),
        Some(&serde_json::json!("context"))
    );
    assert_eq!(
        json.pointer("/project/name"),
        Some(&serde_json::json!("shop"))
    );
    assert_eq!(json.pointer("/next"), None);
}

#[test]
fn signed_out_with_direct_connection_keeps_the_link_hint() {
    let args = ["--connect", "tcp://127.0.0.1:1"];
    let human = ctx_with(false, None, &args, None, None, &[], false);
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("servers = via connect\n"), "{stdout}");
    assert_eq!(
        String::from_utf8(human.stderr).unwrap(),
        "next: ployz link --project PROJECT\n"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&ctx_with(true, None, &args, None, None, &[], false).stdout)
            .unwrap();
    assert_eq!(
        json.pointer("/servers/via"),
        Some(&serde_json::json!("connect"))
    );
    assert_eq!(
        json.pointer("/next"),
        Some(&serde_json::json!("ployz link --project PROJECT"))
    );
}

#[test]
fn signed_out_with_context_override_and_project_has_no_hint() {
    let config = STANDALONE_CONTEXT.replacen("current_context: lab\n", "", 1);
    let environment = [("PLOYZ_CONTEXT", "lab")];
    let human = ctx_with(false, None, &[], Some(&config), None, &environment, true);
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("project = shop (link)\n"), "{stdout}");
    assert!(stdout.contains("servers = via context lab\n"), "{stdout}");
    assert_eq!(String::from_utf8(human.stderr).unwrap(), "");
    let json: serde_json::Value = serde_json::from_slice(
        &ctx_with(true, None, &[], Some(&config), None, &environment, true).stdout,
    )
    .unwrap();
    assert_eq!(
        json.pointer("/servers/context"),
        Some(&serde_json::json!("lab"))
    );
    assert_eq!(
        json.pointer("/project/name"),
        Some(&serde_json::json!("shop"))
    );
    assert_eq!(json.pointer("/next"), None);
}

#[test]
fn stored_device_login_keeps_organization_and_link_hint() {
    let cloud = r#"{"state":"signed_in","cloud":"http://127.0.0.1:9","token":"example-token","account":{"id":"user-1","email":"user@example.com","name":"Example"},"organization":{"id":"org-1","slug":"example"}}"#;
    let human = ctx_with(false, None, &[], None, Some(cloud), &[], false);
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert!(stdout.contains("organization = example\n"), "{stdout}");
    assert_eq!(
        String::from_utf8(human.stderr).unwrap(),
        "next: ployz link --project PROJECT\n"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&ctx_with(true, None, &[], None, Some(cloud), &[], false).stdout)
            .unwrap();
    assert_eq!(
        json.pointer("/organization/slug"),
        Some(&serde_json::json!("example"))
    );
    assert_eq!(
        json.pointer("/credential"),
        Some(&serde_json::json!("device"))
    );
    assert_eq!(
        json.pointer("/next"),
        Some(&serde_json::json!("ployz link --project PROJECT"))
    );
}
