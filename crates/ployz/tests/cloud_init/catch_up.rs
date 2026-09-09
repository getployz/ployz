//! Shared post-join Global catch-up behavior for Cloud enrollment and Machine add.
//! Retry exhaustion and exact budgets run against the real Client under virtual
//! time in global_catch_up_tests; these CLI cases cover recovery and reporting.

use std::{collections::BTreeMap, fs, process::Output};

use super::harness::{
    EnrollListen, JoinDaemon, PAIRING, RelayListen, TOKEN, founder_machine, ingress_on,
    registration, serve_machine,
};
use ployz::context::{Config, Connection, Context};
use ployz_core::{
    CloudPairing, ContainerId, ContainerObservation, MembershipObservation, PairingCredential,
    ProjectName, ServiceId, ServiceName,
};
use serde_json::json;

#[derive(Clone, Copy)]
enum Fault {
    Healthy,
    Transient,
    Permanent,
}

fn with_faults(daemon: JoinDaemon, target: Fault, ensure: Fault) -> JoinDaemon {
    let daemon = match target {
        Fault::Healthy => daemon,
        Fault::Transient => daemon.transient_target_inspect_failures(1),
        Fault::Permanent => daemon.fail_target_inspect(),
    };
    match ensure {
        Fault::Healthy => daemon,
        Fault::Transient => daemon.transient_ensure_failures(1),
        Fault::Permanent => daemon.fail_ensure(),
    }
}

#[tokio::test]
async fn cloud_join_retries_target_readiness_and_reports_failure() {
    let (recovered, daemon) = cloud_join(Fault::Transient, Fault::Healthy).await;
    assert!(recovered.status.success());
    assert_eq!(daemon.target_inspect_attempts(), 2);

    let (failed, daemon) = cloud_join(Fault::Permanent, Fault::Healthy).await;
    assert!(!failed.status.success());
    assert_eq!(daemon.target_inspect_attempts(), 1);
    assert_eq!(daemon.ensure_attempts(), 0);
    assert_joined_with_incomplete_catch_up(&failed);
    daemon.join_request();
}

#[tokio::test]
async fn machine_add_retries_target_readiness_and_reports_failure() {
    let (recovered, entry, target) = machine_add(Fault::Transient, Fault::Healthy).await;
    assert!(recovered.status.success());
    assert_eq!(entry.target_inspect_attempts(), 2);
    target.join_request();

    let (failed, entry, target) = machine_add(Fault::Permanent, Fault::Healthy).await;
    assert!(!failed.status.success());
    assert_eq!(entry.target_inspect_attempts(), 1);
    assert_eq!(entry.ensure_attempts(), 0);
    assert!(String::from_utf8_lossy(&failed.stdout).contains("Added Machine joiner"));
    assert_joined_with_incomplete_catch_up(&failed);
    target.join_request();
}

#[tokio::test]
async fn cloud_join_retries_catch_up_and_reports_failure() {
    let (recovered, daemon) = cloud_join(Fault::Healthy, Fault::Transient).await;
    assert!(recovered.status.success());
    assert_eq!(daemon.ensure_attempts(), 3);

    let (failed, daemon) = cloud_join(Fault::Healthy, Fault::Permanent).await;
    assert!(!failed.status.success());
    assert_eq!(daemon.ensure_attempts(), 2);
    assert_joined_with_incomplete_catch_up(&failed);
    daemon.join_request();
}

#[tokio::test]
async fn machine_add_retries_catch_up_and_reports_failure() {
    let (recovered, entry, target) = machine_add(Fault::Healthy, Fault::Transient).await;
    assert!(recovered.status.success());
    assert_eq!(entry.ensure_attempts(), 3);
    target.join_request();

    let (failed, entry, target) = machine_add(Fault::Healthy, Fault::Permanent).await;
    assert!(!failed.status.success());
    assert_eq!(entry.ensure_attempts(), 2);
    assert!(String::from_utf8_lossy(&failed.stdout).contains("Added Machine joiner"));
    assert_joined_with_incomplete_catch_up(&failed);
    target.join_request();
}

#[tokio::test]
async fn machine_add_reports_omitted_targets_before_catch_up() {
    for membership in [MembershipObservation::Down, MembershipObservation::Unknown] {
        let (output, entry, target) =
            machine_add_with_membership(Fault::Healthy, Fault::Healthy, membership).await;
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("partial Service observations"), "{stderr}");
        assert!(stderr.contains("no terminal response"), "{stderr}");
        assert!(stderr.contains("remains a Cluster member"), "{stderr}");
        assert_eq!(entry.target_inspect_attempts(), 0);
        assert_eq!(entry.ensure_attempts(), 0);
        target.join_request();
    }
}

async fn cloud_join(target_failures: Fault, ensure_failures: Fault) -> (Output, JoinDaemon) {
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
    let daemon = with_faults(
        JoinDaemon::new(registration).with_containers(globals_on(&founder)),
        target_failures,
        ensure_failures,
    );
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
    target_failures: Fault,
    ensure_failures: Fault,
) -> (Output, JoinDaemon, JoinDaemon) {
    machine_add_with_membership(target_failures, ensure_failures, MembershipObservation::Up).await
}

async fn machine_add_with_membership(
    target_failures: Fault,
    ensure_failures: Fault,
    membership: MembershipObservation,
) -> (Output, JoinDaemon, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let entry = with_faults(
        JoinDaemon::new(registration.clone())
            .with_containers(globals_on(&founder))
            .with_membership(membership),
        target_failures,
        ensure_failures,
    );
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
    assert!(stderr.contains("ployz ingress deploy"), "stderr: {stderr}");
    assert!(stderr.contains("shop/worker"), "stderr: {stderr}");
    assert!(
        stderr.contains("redeploy Project Service `shop/worker`"),
        "stderr: {stderr}"
    );
}

fn globals_on(machine: &ployz_core::Machine) -> Vec<ContainerObservation> {
    let ingress = ingress_on(machine);
    let mut worker = ingress.clone();
    worker
        .try_update(|parts| {
            parts.container_id = ContainerId::parse("d".repeat(64)).unwrap();
            parts.display_name = "worker-a".into();
            parts.project_name = ProjectName::parse("shop").unwrap();
        })
        .unwrap();
    worker
        .try_update(|parts| {
            parts.resolved_spec.service_id = ServiceId::parse("e".repeat(32)).unwrap()
        })
        .unwrap();
    worker
        .try_update(|parts| parts.resolved_spec.name = ServiceName::parse("worker").unwrap())
        .unwrap();
    vec![ingress, worker]
}
