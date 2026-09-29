//! `ployz server add` and `ployz cloud reset` as a signed-in device.

use std::path::Path;

use ployz_core::Registered;
use serde_json::{Value, json};

use super::harness::{
    self, EnrollListen, JoinDaemon, PAIRING, TOKEN, founder_machine, serve_machine,
};

const ENROLLMENT: &str = "0b7c1a52-2d3e-4a4f-9d6b-3f1a2b3c4d5e";

/// A signed-in device: `cloud.json` beside the config names `cloud` and Organization `acme`.
fn sign_in(directory: &Path, cloud: &str) -> String {
    std::fs::write(
        directory.join("cloud.json"),
        json!({
            "state": "signed_in",
            "cloud": cloud,
            "token": "session-token",
            "account": { "id": "u1", "email": "nick@example.com", "name": "Nick" },
            "organization": { "id": "o1", "slug": "acme" },
        })
        .to_string(),
    )
    .unwrap();
    directory.join("config.yaml").to_str().unwrap().to_owned()
}

fn minted() -> Value {
    json!({ "id": ENROLLMENT, "token": TOKEN, "expiresAt": "2026-09-30T00:00:00.000Z" })
}

fn stdout_json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[tokio::test]
async fn signed_in_server_add_founds_the_cluster_over_ssh_with_a_minted_token() {
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "resumed": false,
        "storage": "none",
        "pairing": { "secret": PAIRING },
    }))
    .await;
    enroll.reply_cli("/api/cli/servers/enroll", [minted()]);
    let founder = founder_machine();
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder.clone(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let address = serve_machine(daemon.clone()).await;
    let directory = tempfile::tempdir().unwrap();
    let config = sign_in(directory.path(), &enroll.url);

    let output = harness::cli()
        .args([
            "--ployz-config",
            &config,
            "--json",
            "server",
            "add",
            &format!("ssh://root@{address}"),
            "--no-install",
            "--name",
            "founder",
            "--accepts-ingress=false",
            "--yes",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = stdout_json(&output);
    assert_eq!(result.pointer("/founded"), Some(&json!(true)));
    assert_eq!(result.pointer("/server/id"), Some(&json!(founder.id)));
    assert_eq!(daemon.initialize_request().name.as_str(), "founder");
    assert_eq!(
        enroll.cli_calls(),
        [(
            "/api/cli/servers/enroll".to_owned(),
            "session-token".to_owned(),
            json!({ "organizationSlug": "acme" })
        )]
    );
    assert_eq!(enroll.paths().get(1), Some(&format!("/api/enroll/{TOKEN}")));
}

#[tokio::test]
async fn server_add_without_a_sign_in_fails_fast_with_the_login_hint() {
    let directory = tempfile::tempdir().unwrap();
    let output = harness::cli()
        .args([
            "--ployz-config",
            directory.path().join("config.yaml").to_str().unwrap(),
            "--json",
            "server",
            "add",
            "root@203.0.113.1",
        ])
        .output()
        .await
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let error = stdout_json(&output);
    assert_eq!(
        error.pointer("/error/code"),
        Some(&json!("unauthenticated"))
    );
    assert_eq!(
        error.pointer("/error/details/next"),
        Some(&json!("ployz login"))
    );
}

#[tokio::test]
async fn pasted_command_returns_at_once_under_json_and_wait_reports_the_joined_server() {
    let cloud = EnrollListen::script([]).await;
    cloud.reply_cli("/api/cli/servers/enroll", [minted()]);
    cloud.reply_cli(
        "/api/cli/servers/enrollment",
        [
            json!({ "status": "pending" }),
            json!({ "status": "joined", "machineId": "a".repeat(32) }),
        ],
    );
    let directory = tempfile::tempdir().unwrap();
    let config = sign_in(directory.path(), &cloud.url);

    let pending = harness::cli()
        .args([
            "--ployz-config",
            &config,
            "--json",
            "server",
            "add",
            "--command",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        pending.status.success(),
        "{}",
        String::from_utf8_lossy(&pending.stderr)
    );
    let pending = stdout_json(&pending);
    assert_eq!(pending.pointer("/status"), Some(&json!("pending")));
    assert_eq!(pending.pointer("/enrollment"), Some(&json!(ENROLLMENT)));
    assert_eq!(
        pending.pointer("/next"),
        Some(&json!(format!("ployz server add --wait {ENROLLMENT}")))
    );
    assert_eq!(
        pending.pointer("/command"),
        Some(&json!(format!(
            "curl -fsSL https://ployz.sh/ | sh -s -- {} && sudo ployz server add --token '{TOKEN}' --cloud-url '{}'",
            env!("CARGO_PKG_VERSION"),
            cloud.url
        )))
    );

    let joined = harness::cli()
        .args([
            "--ployz-config",
            &config,
            "--json",
            "server",
            "add",
            "--wait",
            ENROLLMENT,
        ])
        .output()
        .await
        .unwrap();
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    assert_eq!(
        stdout_json(&joined),
        json!({ "server": { "id": "a".repeat(32) }, "status": "joined" })
    );
    assert_eq!(
        cloud.cli_calls().last().map(|call| call.2.clone()),
        Some(json!({ "organizationSlug": "acme", "id": ENROLLMENT }))
    );
}

#[tokio::test]
async fn cloud_reset_gives_up_the_founding_of_the_signed_in_organization() {
    let cloud = EnrollListen::script([]).await;
    cloud.reply_cli("/api/cli/cloud/reset", [json!({ "reset": true })]);
    let directory = tempfile::tempdir().unwrap();
    let config = sign_in(directory.path(), &cloud.url);

    let output = harness::cli()
        .args([
            "--ployz-config",
            &config,
            "--json",
            "cloud",
            "reset",
            "--yes",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout_json(&output).pointer("/reset"), Some(&json!(true)));
    assert_eq!(
        cloud.cli_calls(),
        [(
            "/api/cli/cloud/reset".to_owned(),
            "session-token".to_owned(),
            json!({ "organizationSlug": "acme", "confirmedFounderStoppedOrErased": true })
        )]
    );
}

#[tokio::test]
async fn cloud_reset_with_ployz_token_acts_in_the_tokens_organization() {
    let cloud = EnrollListen::script([]).await;
    cloud.reply_cli(
        "/api/cli/organizations",
        [json!({ "organizations": [
            { "id": "o1", "slug": "acme", "name": "Acme", "current": false },
            { "id": "o2", "slug": "beta", "name": "Beta", "current": true },
        ] })],
    );
    cloud.reply_cli("/api/cli/cloud/reset", [json!({ "reset": true })]);
    let directory = tempfile::tempdir().unwrap();
    // Signed in to acme, but PLOYZ_TOKEN wins, as it does for every Cloud command.
    let config = sign_in(directory.path(), &cloud.url);

    let output = harness::cli()
        .env("PLOYZ_TOKEN", "ployz_beta")
        .env("PLOYZ_CLOUD_URL", &cloud.url)
        .args([
            "--ployz-config",
            &config,
            "--json",
            "cloud",
            "reset",
            "--yes",
        ])
        .output()
        .await
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        cloud.cli_calls().last(),
        Some(&(
            "/api/cli/cloud/reset".to_owned(),
            "ployz_beta".to_owned(),
            json!({ "organizationSlug": "beta", "confirmedFounderStoppedOrErased": true })
        ))
    );
}
