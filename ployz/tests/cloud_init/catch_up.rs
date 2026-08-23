//! Shared post-join Global catch-up behavior for Cloud enrollment and Machine add.

use std::{collections::BTreeMap, fs, process::Output};

use super::harness::{
    EnrollListen, JoinDaemon, PAIRING, RelayListen, TOKEN, caddy_on, founder_machine, registration,
    serve_machine,
};
use ployz::context::{Config, Connection, Context};
use ployz_core::{
    CloudPairing, ContainerId, ContainerObservation, MembershipObservation, PairingCredential,
    ProjectName, ServiceId, ServiceName,
};
use serde_json::json;

#[tokio::test]
async fn cloud_join_retries_target_readiness_and_reports_exhaustion() {
    let (recovered, daemon) = cloud_join(1, 0).await;
    assert!(recovered.status.success());
    assert_eq!(daemon.target_inspect_attempts(), 2);

    let (exhausted, daemon) = cloud_join(4, 0).await;
    assert!(!exhausted.status.success());
    assert_eq!(daemon.target_inspect_attempts(), 4);
    assert_eq!(daemon.ensure_attempts(), 0);
    assert_joined_with_incomplete_catch_up(&exhausted);
    daemon.join_request();
}

#[tokio::test]
async fn machine_add_retries_target_readiness_and_reports_exhaustion() {
    let (recovered, entry, target) = machine_add(1, 0).await;
    assert!(recovered.status.success());
    assert_eq!(entry.target_inspect_attempts(), 2);
    target.join_request();

    let (exhausted, entry, target) = machine_add(4, 0).await;
    assert!(!exhausted.status.success());
    assert_eq!(entry.target_inspect_attempts(), 4);
    assert_eq!(entry.ensure_attempts(), 0);
    assert!(String::from_utf8_lossy(&exhausted.stdout).contains("Added Machine joiner"));
    assert_joined_with_incomplete_catch_up(&exhausted);
    target.join_request();
}

#[tokio::test]
async fn cloud_join_retries_catch_up_and_reports_exhaustion() {
    let (recovered, daemon) = cloud_join(0, 1).await;
    assert!(recovered.status.success());
    assert_eq!(daemon.ensure_attempts(), 3);

    let (exhausted, daemon) = cloud_join(0, 8).await;
    assert!(!exhausted.status.success());
    assert_eq!(daemon.ensure_attempts(), 8);
    assert_joined_with_incomplete_catch_up(&exhausted);
    daemon.join_request();
}

#[tokio::test]
async fn machine_add_retries_catch_up_and_reports_exhaustion() {
    let (recovered, entry, target) = machine_add(0, 1).await;
    assert!(recovered.status.success());
    assert_eq!(entry.ensure_attempts(), 3);
    target.join_request();

    let (exhausted, entry, target) = machine_add(0, 8).await;
    assert!(!exhausted.status.success());
    assert_eq!(entry.ensure_attempts(), 8);
    assert!(String::from_utf8_lossy(&exhausted.stdout).contains("Added Machine joiner"));
    assert_joined_with_incomplete_catch_up(&exhausted);
    target.join_request();
}

#[tokio::test]
async fn machine_add_verifies_target_while_membership_is_not_up() {
    for membership in [MembershipObservation::Down, MembershipObservation::Unknown] {
        let (output, entry, target) = machine_add_with_membership(0, 0, membership).await;
        assert!(output.status.success(), "stderr: {:?}", output.stderr);
        assert_eq!(entry.target_inspect_attempts(), 1);
        assert_eq!(entry.ensure_attempts(), 2);
        target.join_request();
    }
}

async fn cloud_join(target_failures: usize, ensure_failures: usize) -> (Output, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration)
        .with_containers(globals_on(&founder))
        .transient_target_inspect_failures(target_failures)
        .transient_ensure_failures(ensure_failures);
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

async fn machine_add(
    target_failures: usize,
    ensure_failures: usize,
) -> (Output, JoinDaemon, JoinDaemon) {
    machine_add_with_membership(target_failures, ensure_failures, MembershipObservation::Up).await
}

async fn machine_add_with_membership(
    target_failures: usize,
    ensure_failures: usize,
    membership: MembershipObservation,
) -> (Output, JoinDaemon, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let entry = JoinDaemon::new(registration.clone())
        .with_containers(globals_on(&founder))
        .with_membership(membership)
        .transient_target_inspect_failures(target_failures)
        .transient_ensure_failures(ensure_failures);
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
    assert!(stderr.contains("shop/worker"), "stderr: {stderr}");
    assert!(
        stderr.contains("redeploy Project Service `shop/worker`"),
        "stderr: {stderr}"
    );
}

fn globals_on(machine: &ployz_core::Machine) -> Vec<ContainerObservation> {
    let caddy = caddy_on(machine);
    let mut worker = caddy.clone();
    worker.container_id = ContainerId::parse("d".repeat(64)).unwrap();
    worker.display_name = "worker-a".into();
    worker.project_name = ProjectName::parse("shop").unwrap();
    worker.service_id = ServiceId::parse("e".repeat(32)).unwrap();
    worker.service_name = ServiceName::parse("worker").unwrap();
    worker.resolved_spec.service_id = worker.service_id;
    worker.resolved_spec.name = worker.service_name.clone();
    vec![caddy, worker]
}
