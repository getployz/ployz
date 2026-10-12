//! The fake fleet as the eval sees it: through the SDK's unix `Connection` seam.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use ployz::{
    connect::{Client, SystemConnector, connect_selected_with},
    context::{Connection, ConnectionSource, SelectedConnections},
    sdk::{self, ContainerLogInput},
};
use ployz_core::{
    MachineName, MachineRelease, MachineTarget, MachineUpgradeAttemptId, MachineVersion,
    RequestMachineUpgradeRequest, StopContainerRequest, op,
};
use ployz_testkit::fake_fleet::{
    FakeFleet, FakeFleetError, FleetFile, LedgerEntry, Scenario, container_id,
};
use serde_json::{Value, json};

fn fixture() -> FleetFile {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../dashboard/evals/agent/machines.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn name(value: &str) -> MachineName {
    MachineName::parse(value).unwrap()
}

fn scenario(value: Value) -> Scenario {
    serde_json::from_value(value).unwrap()
}

fn start(scenario: Scenario) -> (FakeFleet, tempfile::TempDir) {
    let sockets = tempfile::tempdir().unwrap();
    let fleet = FakeFleet::start(fixture(), scenario, sockets.path(), None).unwrap();
    (fleet, sockets)
}

async fn session(fleet: &FakeFleet, machine: &str) -> Result<sdk::Session, ployz_core::RpcError> {
    sdk::connect_connections(
        vec![Connection::unix(fleet.socket(&name(machine)).unwrap()).unwrap()],
        Arc::new(SystemConnector::default()),
    )
    .await
}

async fn client(fleet: &FakeFleet, machine: &str) -> Client {
    connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::unix(fleet.socket(&name(machine)).unwrap()).unwrap()],
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap()
}

fn stop(container: &str) -> StopContainerRequest {
    StopContainerRequest {
        container_id: container_id(container),
        signal: None,
        grace_period_seconds: None,
    }
}

fn commands(ledger: &[LedgerEntry]) -> Vec<(String, String)> {
    ledger
        .iter()
        .map(|entry| (entry.machine.to_string(), entry.command.clone()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn online_machines_confirm_their_contract_and_offline_ones_have_no_socket() {
    let (fleet, _sockets) = start(Scenario::default());

    for machine in ["web-1", "web-2"] {
        let about = session(&fleet, machine)
            .await
            .unwrap()
            .about()
            .await
            .unwrap();
        let expected = fixture()
            .machines
            .into_iter()
            .find(|candidate| candidate.name.as_str() == machine)
            .unwrap();
        assert_eq!(about.machine_id, expected.id);
        assert_eq!(about.daemon_version, "0.4.0");
    }
    assert!(!fleet.socket(&name("web-3")).unwrap().exists());
    assert!(session(&fleet, "web-3").await.is_err());

    fleet.online(&name("web-3")).unwrap();
    let about = session(&fleet, "web-3")
        .await
        .unwrap()
        .about()
        .await
        .unwrap();
    assert_eq!(about.daemon_version, "0.4.0");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_stop_reply_is_retried_and_both_attempts_are_in_the_ledger() {
    let (fleet, _sockets) = start(scenario(json!({
        "faults": [{"fault": "drop_reply", "machine": "web-1", "command": "stop_container", "nth": 1}]
    })));
    let mut client = client(&fleet, "web-1").await;
    let before = fleet.ledger().unwrap().len();

    client
        .call::<op::StopContainer>(
            stop("app-production-web-1"),
            Some(&MachineTarget::parse("web-1").unwrap()),
        )
        .await
        .unwrap();

    let after = fleet.ledger().unwrap();
    let issued = commands(after.get(before..).unwrap());
    let stops = after
        .iter()
        .filter(|entry| entry.command == "stop_container")
        .collect::<Vec<_>>();
    // The SDK sees Unavailable, redials (confirming the contract) and resends the stop.
    assert_eq!(
        issued,
        [
            ("web-1".to_owned(), "stop_container".to_owned()),
            ("web-1".to_owned(), "describe_contract".to_owned()),
            ("web-1".to_owned(), "stop_container".to_owned()),
        ]
    );
    assert_eq!(stops.len(), 2);
    assert!(
        stops
            .iter()
            .all(|entry| entry.container.as_deref() == Some("app-production-web-1"))
    );
    assert!(
        !fleet
            .is_running(&name("web-1"), "app-production-web-1")
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnect_after_a_web_2_stop_leaves_web_2_offline() {
    let (fleet, _sockets) = start(scenario(json!({
        "faults": [{"fault": "disconnect_after", "machine": "web-2", "command": "stop_container"}]
    })));
    let socket = fleet.socket(&name("web-2")).unwrap();
    let mut client = client(&fleet, "web-1").await;

    let result = client
        .call::<op::StopContainer>(
            stop("app-production-web-2"),
            Some(&MachineTarget::parse("web-2").unwrap()),
        )
        .await;

    assert!(result.is_err());
    assert!(!fleet.is_online(&name("web-2")).unwrap());
    assert!(!socket.exists());
    assert!(
        !fleet
            .is_running(&name("web-2"), "app-production-web-2")
            .unwrap()
    );
    let stops = fleet
        .ledger()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.command == "stop_container")
        .count();
    assert_eq!(stops, 1, "retries to an offline Machine are not recorded");
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_stream_the_fixture_lines_and_honor_tail() {
    let (fleet, _sockets) = start(Scenario::default());
    let web_1 = fixture().machines.into_iter().next().unwrap();
    let expected = web_1
        .containers
        .iter()
        .find(|container| container.name == "app-staging-web-1")
        .unwrap()
        .logs
        .clone();
    let session = session(&fleet, "web-1").await.unwrap();

    for (tail, wanted) in [
        (-1, expected.as_slice()),
        (3, &expected[expected.len() - 3..]),
    ] {
        let stream = session
            .container_logs(ContainerLogInput {
                machine_id: web_1.id,
                container_id: container_id("app-staging-web-1"),
                tail,
                follow: false,
                since_unix_seconds: None,
            })
            .await
            .unwrap();
        let mut lines = Vec::new();
        while let Some(record) = stream.next().await.unwrap() {
            lines.push(record.message);
        }
        assert_eq!(lines, wanted);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_resent_with_its_attempt_id_creates_one_attempt() {
    let (fleet, _sockets) = start(Scenario::default());
    let session = session(&fleet, "web-1").await.unwrap();
    let attempt_id = MachineUpgradeAttemptId::parse("0123456789abcdef0123456789abcdef").unwrap();
    let target = MachineVersion::parse("0.5.0").unwrap();
    let request = || RequestMachineUpgradeRequest {
        attempt_id,
        release: MachineRelease::Exact(target.clone()),
    };

    let first = session
        .request_machine_upgrade("web-1", request())
        .await
        .unwrap();
    let again = session
        .request_machine_upgrade("web-1", request())
        .await
        .unwrap();

    assert_eq!(first, again);
    assert_eq!(first.target, target);
    assert_eq!(session.inspect().await.unwrap().upgrade, Some(first));
    assert_eq!(session.about().await.unwrap().daemon_version, "0.5.0");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unmodeled_command_is_recorded_and_answers_unimplemented() {
    let (fleet, _sockets) = start(Scenario::default());
    let mut client = client(&fleet, "web-1").await;

    let error = client
        .call::<op::RemoveContainer>(
            ployz_core::RemoveContainerRequest {
                container_id: container_id("app-staging-web-1"),
                force: false,
                remove_volumes: false,
            },
            None,
        )
        .await
        .unwrap_err();

    let message = format!("{error:?}");
    assert!(message.contains("Unimplemented"), "{message}");
    assert!(message.contains("remove_container"), "{message}");
    let last = fleet.ledger().unwrap().pop().unwrap();
    assert_eq!(last.command, "remove_container");
    assert_eq!(last.container.as_deref(), Some("app-staging-web-1"));
}

#[test]
fn a_scenario_naming_an_unknown_machine_volume_or_command_is_refused() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let sockets = tempfile::tempdir().unwrap();
    let refused = |value: Value| {
        FakeFleet::start(fixture(), scenario(value), sockets.path(), None)
            .err()
            .unwrap()
    };

    assert!(matches!(
        refused(
            json!({"faults": [{"fault": "disconnect_after", "machine": "web-9", "command": "stop_container"}]})
        ),
        FakeFleetError::UnknownMachine(_)
    ));
    assert!(matches!(
        refused(json!({"faults": [{"fault": "disconnect_after", "machine": "web-1", "command": "reboot"}]})),
        FakeFleetError::UnknownCommand(command) if command == "reboot"
    ));
    assert!(matches!(
        refused(
            json!({"drop_volumes": [{"namespace": "app-staging", "volume": "nope", "machine": "web-1"}]})
        ),
        FakeFleetError::UnknownVolume { .. }
    ));
}

#[test]
fn the_bin_serves_the_fleet_and_answers_control_lines() {
    let sockets = tempfile::tempdir().unwrap();
    let ledger = sockets.path().join("ledger.jsonl");
    let machines =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../dashboard/evals/agent/machines.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ployz-fake-fleet"))
        .arg("--machines")
        .arg(&machines)
        .arg("--sockets")
        .arg(sockets.path())
        .arg("--ledger")
        .arg(&ledger)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut read = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    let web_3 = socket_of(sockets.path(), "web-3");

    assert_eq!(read(), json!({"ready": true}));
    assert!(socket_of(sockets.path(), "web-1").exists());
    assert!(!web_3.exists());
    writeln!(stdin, r#"{{"online":"web-3"}}"#).unwrap();
    assert_eq!(read(), json!({"ok": true}));
    assert!(web_3.exists());
    writeln!(stdin, r#"{{"offline":"web-9"}}"#).unwrap();
    assert_eq!(read()["ok"], json!(false));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let session = sdk::connect_connections(
            vec![Connection::unix(socket_of(sockets.path(), "web-3")).unwrap()],
            Arc::new(SystemConnector::default()),
        )
        .await
        .unwrap();
        session.about().await.unwrap();
    });
    writeln!(stdin, r#"{{"offline":"web-3"}}"#).unwrap();
    assert_eq!(read(), json!({"ok": true}));
    assert!(!web_3.exists());

    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the bin outlived its stdin");
        std::thread::sleep(Duration::from_millis(50));
    }
    let entries = std::fs::read_to_string(&ledger)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<LedgerEntry>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .all(|entry| entry.machine.as_str() == "web-3")
    );
    assert_eq!(entries.first().unwrap().seq, 1);
}

fn socket_of(directory: &Path, machine: &str) -> PathBuf {
    let id = fixture()
        .machines
        .into_iter()
        .find(|candidate| candidate.name.as_str() == machine)
        .unwrap()
        .id;
    directory.join(format!("{id}.sock"))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_watch_sends_the_fleet_once_and_inspect_returns_the_env() {
    let (fleet, _sockets) = start(scenario(json!({
        "mirrors": [{"namespace": "app-production", "volume": "pgdata", "machine": "web-2"}]
    })));
    let session = session(&fleet, "web-1").await.unwrap();
    let watch = session.watch().await.unwrap();

    let frame = watch.next().await.unwrap().unwrap();
    let down = frame
        .machines
        .iter()
        .filter(|machine| machine.membership == ployz_core::MembershipObservation::Down)
        .map(|machine| machine.machine.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(down, ["web-3"]);
    assert!(
        frame
            .containers
            .iter()
            .all(|container| !container.display_name.ends_with("web-3"))
    );
    assert!(
        frame
            .volumes
            .iter()
            .any(|volume| volume.id.name.as_str() == "app-production_pgdata"
                && volume.id.machine_id != fleet_id("web-1"))
    );
    let second = tokio::time::timeout(Duration::from_millis(300), watch.next()).await;
    assert!(second.is_err(), "the watch holds after its one frame");

    let fixture = fixture();
    let env = &fixture.machines[0].containers[0].env;
    let details = session
        .inspect_container(
            "web-1",
            container_id(&fixture.machines[0].containers[0].name).as_str(),
        )
        .await
        .unwrap();
    assert_eq!(details["environment"], serde_json::to_value(env).unwrap());
}

fn fleet_id(machine: &str) -> ployz_core::MachineId {
    fixture()
        .machines
        .into_iter()
        .find(|candidate| candidate.name.as_str() == machine)
        .unwrap()
        .id
}
