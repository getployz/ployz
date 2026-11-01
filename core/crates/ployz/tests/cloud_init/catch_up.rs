//! Shared post-join Global catch-up behavior for Cloud enrollment and Machine add.
//! Retry exhaustion and exact budgets run against the real Client under virtual
//! time in global_catch_up_tests; these CLI cases cover recovery and reporting.

use std::{collections::BTreeMap, fs, process::Output};

use super::harness::{
    EnrollListen, EventLog, JoinDaemon, PAIRING, founder_machine, ingress_on, registration,
    serve_machine,
};
use ployz::context::{Config, Connection, Context};
use ployz_core::{
    ContainerId, ContainerObservation, Machine, MachineId, MachineName, MachineObservation,
    MembershipObservation, Namespace, ServiceId, ServiceName, WireGuardPublicKey,
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
async fn cloud_join_finishes_rpc_work_before_publishing_the_rotated_capability() {
    let (output, _) = cloud_join(Fault::Healthy, Fault::Healthy).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn cloud_join_retries_target_readiness_and_reports_failure() {
    let (recovered, daemon) = cloud_join(Fault::Transient, Fault::Healthy).await;
    assert!(recovered.status.success());
    assert_eq!(daemon.target_inspect_attempts(), 5);

    let (failed, daemon) = cloud_join(Fault::Permanent, Fault::Healthy).await;
    assert!(!failed.status.success());
    assert_eq!(daemon.target_inspect_attempts(), 2);
    assert_eq!(daemon.ensure_attempts(), 0);
    assert_joined_with_incomplete_catch_up(&failed);
    daemon.join_request();
}

#[tokio::test]
async fn machine_add_retries_target_readiness_and_reports_failure() {
    let (recovered, entry, target) = machine_add(Fault::Transient, Fault::Healthy).await;
    assert!(recovered.status.success());
    assert_eq!(entry.target_inspect_attempts(), 5);
    target.join_request();

    let (failed, entry, target) = machine_add(Fault::Permanent, Fault::Healthy).await;
    assert!(!failed.status.success());
    assert_eq!(entry.target_inspect_attempts(), 2);
    assert_eq!(entry.ensure_attempts(), 0);
    assert!(String::from_utf8_lossy(&failed.stdout).contains("Added Server joiner"));
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
    assert!(String::from_utf8_lossy(&failed.stdout).contains("Added Server joiner"));
    assert_joined_with_incomplete_catch_up(&failed);
    target.join_request();
}

#[tokio::test]
async fn machine_add_reports_omitted_targets_before_catch_up() {
    for membership in [MembershipObservation::Down, MembershipObservation::Unknown] {
        let (output, entry, target) = machine_add_with(
            Fault::Healthy,
            Fault::Healthy,
            |entry| entry.with_membership(membership),
            &[],
        )
        .await;
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

fn unarrived_registration(entry: JoinDaemon) -> JoinDaemon {
    entry.listing(MachineObservation::new(
        Machine {
            id: MachineId::parse("c".repeat(32)).unwrap(),
            name: MachineName::parse("claim-only").unwrap(),
            subnet: "10.210.3.0/24".parse().unwrap(),
            public_key: WireGuardPublicKey([3; 32]),
            ..founder_machine()
        },
        MembershipObservation::Down,
    ))
}

#[tokio::test]
async fn machine_add_plans_catch_up_past_an_unarrived_registration() {
    let (output, entry, target) =
        machine_add_with(Fault::Healthy, Fault::Healthy, unarrived_registration, &[]).await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(entry.ensure_attempts() > 0);
    assert_unarrived_on_stderr(&stderr);
    target.join_request();

    let (output, _, target) = machine_add_with(
        Fault::Healthy,
        Fault::Healthy,
        unarrived_registration,
        &["--json"],
    )
    .await;
    assert!(output.status.success());
    assert_unarrived_in_result(&output);
    target.join_request();
}

#[tokio::test]
async fn cloud_join_reports_an_unarrived_registration_in_its_result() {
    let (output, _) = cloud_join_with(
        Fault::Healthy,
        Fault::Healthy,
        unarrived_registration,
        &["--json"],
    )
    .await;
    assert!(output.status.success());
    assert_unarrived_in_result(&output);
}

/// A real Down member, or a failed start, still fails catch-up after it skipped the
/// Unarrived Server; the result and the hint still name that Server.
#[tokio::test]
async fn a_failed_catch_up_still_reports_the_unarrived_registration() {
    let down_member = |entry: JoinDaemon| {
        unarrived_registration(entry.with_membership(MembershipObservation::Down))
    };
    for (ensure, entry) in [
        (
            Fault::Healthy,
            &down_member as &dyn Fn(JoinDaemon) -> JoinDaemon,
        ),
        (Fault::Permanent, &unarrived_registration),
    ] {
        for json in [false, true] {
            let args: &[&str] = if json { &["--json"] } else { &[] };
            let (output, _, target) = machine_add_with(Fault::Healthy, ensure, entry, args).await;
            assert_unarrived_in_failure(&output, json);
            target.join_request();
            let (output, _) = cloud_join_with(Fault::Healthy, ensure, entry, args).await;
            assert_unarrived_in_failure(&output, json);
        }
    }
}

fn assert_unarrived_in_failure(output: &Output, json: bool) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("remains a Cluster member"), "{stderr}");
    if json {
        assert!(
            stderr.contains("Server claim-only may still be joining"),
            "{stderr}"
        );
        assert_unarrived_in_result(output);
    } else {
        assert_unarrived_on_stderr(&stderr);
    }
}

/// An Unarrived Server may be one still joining, so the forget command comes only with
/// its condition, never as the next step.
fn assert_unarrived_on_stderr(stderr: &str) {
    assert!(
        stderr.contains("Server claim-only may still be joining"),
        "{stderr}"
    );
    let forget = stderr
        .lines()
        .find(|line| line.contains("keeps showing it down with no version, remove it with `"))
        .unwrap_or_else(|| panic!("{stderr}"));
    assert!(
        forget.contains(&format!("server forget {} --confirm dead", "c".repeat(32))),
        "{stderr}"
    );
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("inspect: ") && line.contains(" server ls")),
        "{stderr}"
    );
    assert!(
        !stderr
            .lines()
            .any(|line| line.starts_with("next: ") && line.contains("server forget")),
        "{stderr}"
    );
}

/// A `--json` join names each Unarrived Server and the command that forgets it.
fn assert_unarrived_in_result(output: &Output) {
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let Some([entry]) = result
        .get("unarrived")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
    else {
        panic!("one Unarrived Server in {result}");
    };
    let id = "c".repeat(32);
    let next = entry
        .get("next")
        .and_then(serde_json::Value::as_str)
        .unwrap();
    assert_eq!(
        entry,
        &json!({ "id": id, "name": "claim-only", "next": next })
    );
    assert!(
        next.contains(&format!("server forget {id} --confirm dead")),
        "{next}"
    );
    ployz::cli::command()
        .try_get_matches_from(shell_words::split(next).unwrap())
        .unwrap();
}

async fn cloud_join(target_failures: Fault, ensure_failures: Fault) -> (Output, JoinDaemon) {
    cloud_join_with(target_failures, ensure_failures, |daemon| daemon, &[]).await
}

async fn cloud_join_with(
    target_failures: Fault,
    ensure_failures: Fault,
    daemon: impl FnOnce(JoinDaemon) -> JoinDaemon,
    extra_args: &[&str],
) -> (Output, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let pairing = json!({ "secret": PAIRING });
    let events = EventLog::default();
    let enroll = EnrollListen::script_recording(
        [json!({
            "kind": "join",
            "storage": "none",
            "pairing": pairing,
            "registration": registration,
        })],
        events.clone(),
    )
    .await;
    let daemon = with_faults(
        daemon(
            JoinDaemon::new(registration)
                .with_containers(globals_on(&founder))
                .revoked_on_publication(events),
        ),
        target_failures,
        ensure_failures,
    );
    let address = serve_machine(daemon.clone()).await;
    let output = super::harness::cli()
        .args([
            "--connect",
            &format!("ssh://root@{address}"),
            "server",
            "add",
            "--token-file",
            super::harness::token_file(),
            "--cloud-url",
            &enroll.url,
            "--name",
            "joiner",
            "--yes",
        ])
        .args(extra_args)
        .output()
        .await
        .unwrap();
    assert_eq!(
        enroll.callbacks().len(),
        1,
        "a committed join must finish Cloud enrollment even when catch-up fails"
    );
    (output, daemon)
}

async fn machine_add(
    target_failures: Fault,
    ensure_failures: Fault,
) -> (Output, JoinDaemon, JoinDaemon) {
    machine_add_with(target_failures, ensure_failures, |entry| entry, &[]).await
}

async fn machine_add_with(
    target_failures: Fault,
    ensure_failures: Fault,
    entry: impl FnOnce(JoinDaemon) -> JoinDaemon,
    extra_args: &[&str],
) -> (Output, JoinDaemon, JoinDaemon) {
    let founder = founder_machine();
    let mut registration = registration();
    registration.visible_peers = vec![founder.clone()];
    let entry = with_faults(
        entry(
            JoinDaemon::new(registration.clone())
                .with_id(founder.id)
                .with_containers(globals_on(&founder)),
        ),
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
        Some("staging".into()),
        BTreeMap::from([(
            "staging".into(),
            Context {
                connections: vec![Connection::tcp("127.0.0.1:1".parse().unwrap())],
            },
        )]),
    )
    .save()
    .unwrap();
    let output = super::harness::cli()
        .args([
            "--ployz-config",
            config.to_str().unwrap(),
            "--connect",
            &format!("ssh://root@{entry_address}"),
            "server",
            "add",
            "--standalone",
            &format!("ssh://root@{target_address}"),
            "--context",
            "staging",
            "--no-install",
            "--name",
            "joiner",
            "--yes",
        ])
        .args(extra_args)
        .output()
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    for hint in stderr.lines().filter_map(|line| {
        line.strip_prefix("retry: ")
            .or_else(|| line.strip_prefix("inspect: "))
            .or_else(|| line.strip_prefix("next: "))
    }) {
        let args = shell_words::split(hint).unwrap();
        let parsed = ployz::cli::command().try_get_matches_from(args).unwrap();
        let mut leaf = &parsed;
        while let Some((_, child)) = leaf.subcommand() {
            leaf = child;
        }
        assert_eq!(
            leaf.get_one::<String>("ployz-config").unwrap(),
            config.to_str().unwrap()
        );
        assert_eq!(leaf.get_one::<String>("context").unwrap(), "staging");
        assert_eq!(
            leaf.get_one::<String>("connect").unwrap(),
            &format!("ssh://root@{entry_address}")
        );
    }
    fs::remove_dir_all(root).unwrap();
    (output, entry, target)
}

fn assert_joined_with_incomplete_catch_up(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Server joined"), "stderr: {stderr}");
    assert!(
        stderr.contains("remains a Cluster member"),
        "stderr: {stderr}"
    );
    assert_ingress_retry(&stderr);
    assert!(stderr.contains("shop/worker"), "stderr: {stderr}");
    assert_eq!(
        stderr
            .matches("redeploy Namespace Service `shop/worker`")
            .count(),
        1,
        "stderr: {stderr}"
    );
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("inspect: ")
                && line.contains("logs shop/worker --machine "))
            .count(),
        1
    );
}

pub(super) fn assert_ingress_retry(stderr: &str) {
    let retry = stderr
        .lines()
        .find_map(|line| line.strip_prefix("retry: "))
        .unwrap_or_else(|| panic!("missing retry command: {stderr}"));
    let retry = shell_words::split(retry).unwrap();
    let parsed = ployz::cli::command().try_get_matches_from(retry).unwrap();
    let Some(("server", group)) = parsed.subcommand() else {
        panic!("retry must address a Server");
    };
    let Some(("set", leaf)) = group.subcommand() else {
        panic!("retry must set its ingress role");
    };
    assert!(ployz_core::MachineId::parse(leaf.get_one::<String>("server").unwrap()).is_ok());
    assert_eq!(leaf.get_one::<bool>("accepts-ingress"), Some(&true));
}

fn globals_on(machine: &ployz_core::Machine) -> Vec<ContainerObservation> {
    let ingress = ingress_on(machine);
    let mut worker = ingress.clone();
    worker
        .try_update(|parts| {
            parts.container_id = ContainerId::parse("d".repeat(64)).unwrap();
            parts.display_name = "worker-a".into();
            parts.namespace = Namespace::parse("shop").unwrap();
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
