//! Façade tests for Cloud's Upgrade calls on one Machine.

use std::sync::atomic::Ordering;
use std::time::Duration;

use ployz_core::{
    MachineRelease, MachineUpgradeAttemptId, MachineUpgradeOutcome, RequestMachineUpgradeRequest,
    RpcErrorCode,
};
use tokio::time::timeout;

use super::support::{DiscoveryService, machine};
use super::unix_session::{self, UnixSession};

#[tokio::test]
async fn a_repeated_upgrade_request_returns_the_same_attempt() {
    let description = super::sdk::advertised_description();
    let session = UnixSession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![machine('b', "worker")];
    service.lose_upgrade_reply.store(true, Ordering::SeqCst);
    let upgrades = service.upgrade_targets.clone();
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    let request = RequestMachineUpgradeRequest {
        attempt_id: MachineUpgradeAttemptId::random(),
        release: MachineRelease::Stable,
    };

    let first = client
        .request_machine_upgrade("worker", request.clone())
        .await
        .unwrap();
    let repeated = client
        .request_machine_upgrade("worker", request.clone())
        .await
        .unwrap();

    assert_eq!(first.attempt_id, request.attempt_id);
    assert_eq!(first.outcome, MachineUpgradeOutcome::Accepted);
    assert_eq!(repeated, first);
    // The lost reply was requested again.
    assert_eq!(*upgrades.lock().unwrap(), ["worker"; 3]);
}

#[tokio::test]
async fn another_attempt_is_refused() {
    let description = super::sdk::advertised_description();
    let session = UnixSession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![machine('b', "worker")];
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    let request = |attempt_id| RequestMachineUpgradeRequest {
        attempt_id,
        release: MachineRelease::Stable,
    };

    client
        .request_machine_upgrade("worker", request(MachineUpgradeAttemptId::random()))
        .await
        .unwrap();
    let other = client
        .request_machine_upgrade("worker", request(MachineUpgradeAttemptId::random()))
        .await
        .unwrap_err();

    assert_eq!(other.code, RpcErrorCode::Conflict);
}

#[tokio::test]
async fn upgrade_cli_describes_same_version() {
    upgrade_cli_case(
        "1.2.3",
        "Server machine-1 already runs 1.2.3.",
        Some("unchanged"),
    )
    .await;
}

#[tokio::test]
async fn upgrade_cli_describes_downgrade() {
    upgrade_cli_case(
        "1.2.4",
        "Downgraded Server machine-1 from 1.2.4 to 1.2.3.",
        Some("downgraded"),
    )
    .await;
}

#[tokio::test]
async fn upgrade_cli_describes_upgrade() {
    upgrade_cli_case(
        "1.2.2",
        "Upgraded Server machine-1 to 1.2.3.",
        Some("upgraded"),
    )
    .await;
}

#[tokio::test]
async fn upgrade_cli_does_not_guess_direction_without_a_prior_version() {
    upgrade_cli_case("", "Server machine-1 now runs 1.2.3.", None).await;
}

async fn upgrade_cli_case(running: &str, phrase: &str, change: Option<&str>) {
    for json in [false, true] {
        let mut service = DiscoveryService::new(super::support::test_description());
        let mut server = machine('a', "machine-1");
        server.machine.runtime.daemon_version = running.into();
        server.machine.accepts_ingress = false;
        service.machines = vec![server];
        service.upgrade_outcome = Some(MachineUpgradeOutcome::Succeeded {
            version: ployz_core::MachineVersion::parse("1.2.3").unwrap(),
        });
        let (address, rpc) = super::support::serve_discovery(service).await;
        let home = tempfile::tempdir().unwrap();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .env("HOME", home.path())
            .env_remove("PLOYZ_TOKEN")
            .args([
                "--connect",
                &format!("tcp://{address}"),
                "server",
                "upgrade",
                "1.2.3",
                "machine-1",
                "--allow-downgrade",
            ]);
        if json {
            command.arg("--json");
        }
        let output = command.output().await.unwrap();
        super::support::cli_transcript(&output);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(0), "{text}");
        if json {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                result.pointer("/attempts/0/from_version"),
                Some(&serde_json::json!(running)),
                "{result}"
            );
            assert_eq!(
                result.pointer("/attempts/0/change"),
                change.map(serde_json::Value::from).as_ref(),
                "{result}"
            );
        } else {
            assert!(text.contains(phrase), "{text}");
            assert!(!text.contains("Everything is up to date"), "{text}");
        }
        rpc.abort();
    }
}

#[tokio::test]
async fn upgrade_cli_reports_a_missing_release_as_not_found() {
    let mut service = DiscoveryService::new(super::support::test_description());
    let mut server = machine('a', "machine-1");
    server.machine.accepts_ingress = false;
    service.machines = vec![server];
    service.upgrade_outcome = Some(MachineUpgradeOutcome::Failed {
        stage: ployz_core::MachineUpgradeStage::Acquiring,
        error: "release selection: no such release 9.9.9".into(),
    });
    let (address, rpc) = super::support::serve_discovery(service).await;
    let home = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env("HOME", home.path())
        .env_remove("PLOYZ_TOKEN")
        .args([
            "--json",
            "--connect",
            &format!("tcp://{address}"),
            "server",
            "upgrade",
            "9.9.9",
            "machine-1",
        ])
        .output()
        .await
        .unwrap();
    super::support::cli_transcript(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{result}");
    assert_eq!(
        result.pointer("/follow_up_error/code"),
        Some(&serde_json::json!("not_found")),
        "{result}"
    );
    assert_eq!(
        result.pointer("/follow_up_error/message"),
        Some(&serde_json::json!("No Ployz release 9.9.9.")),
        "{result}"
    );
    rpc.abort();
}
