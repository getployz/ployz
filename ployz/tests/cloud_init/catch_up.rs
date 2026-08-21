//! Shared post-join Global catch-up behavior for Cloud enrollment and Machine add.

use std::{collections::BTreeMap, fs, process::Output};

use super::harness::{
    EnrollListen, JoinDaemon, PAIRING, RelayListen, TOKEN, caddy_on, founder_machine, registration,
    serve_machine,
};
use ployz::context::{Config, Connection, Context};
use ployz_core::{CloudPairing, PairingCredential};
use serde_json::json;

#[tokio::test]
async fn cloud_join_retries_transient_catch_up_and_reports_exhaustion() {
    let (recovered, daemon) = cloud_join(1).await;
    assert!(recovered.status.success());
    assert_eq!(daemon.ensure_attempts(), 2);

    let (exhausted, daemon) = cloud_join(4).await;
    assert!(!exhausted.status.success());
    assert_eq!(daemon.ensure_attempts(), 4);
    assert_joined_with_incomplete_catch_up(&exhausted);
    daemon.join_request();
}

#[tokio::test]
async fn machine_add_retries_transient_catch_up_and_reports_exhaustion() {
    let (recovered, entry, target) = machine_add(1).await;
    assert!(recovered.status.success());
    assert_eq!(entry.ensure_attempts(), 2);
    target.join_request();

    let (exhausted, entry, target) = machine_add(4).await;
    assert!(!exhausted.status.success());
    assert_eq!(entry.ensure_attempts(), 4);
    assert!(String::from_utf8_lossy(&exhausted.stdout).contains("Added Machine joiner"));
    assert_joined_with_incomplete_catch_up(&exhausted);
    target.join_request();
}

async fn cloud_join(failures: usize) -> (Output, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration)
        .with_containers(vec![caddy_on(&founder)])
        .transient_ensure_failures(failures);
    let address = serve_machine(daemon.clone()).await;
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{address}"),
            "cloud",
            "enroll",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "joiner",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    (output, daemon)
}

async fn machine_add(failures: usize) -> (Output, JoinDaemon, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let entry = JoinDaemon::new(registration.clone())
        .with_containers(vec![caddy_on(&founder)])
        .transient_ensure_failures(failures);
    let target = JoinDaemon::new(registration);
    let entry_address = serve_machine(entry.clone()).await;
    let target_address = serve_machine(target.clone()).await;
    let root = std::env::temp_dir().join(format!(
        "ployz-machine-add-catch-up-{}",
        ployz_core::MachineId::random()
    ));
    let config = root.join("config.yaml");
    Config::new(
        &config,
        Some("test".into()),
        BTreeMap::from([(
            "test".into(),
            Context {
                connections: vec![Connection::tcp(entry_address)],
            },
        )]),
    )
    .save()
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--ployz-config",
            config.to_str().unwrap(),
            "machine",
            "add",
            &format!("tcp://{target_address}"),
            "--no-install",
            "--name",
            "joiner",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    (output, entry, target)
}

fn assert_joined_with_incomplete_catch_up(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Machine joined"), "stderr: {stderr}");
    assert!(
        stderr.contains("remains a Cluster member"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("ployz caddy deploy"), "stderr: {stderr}");
}
