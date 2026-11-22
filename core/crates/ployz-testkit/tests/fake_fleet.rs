//! The fake fleet's own checks: a scenario it refuses, and the bin the agent eval runs.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use hyper_util::rt::TokioIo;
use ployz_core::{DescribeContractRequest, MachineRpcClient, op};
use ployz_testkit::fake_fleet::{FakeFleet, FakeFleetError, FleetFile, LedgerEntry, Scenario};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tonic::transport::Endpoint;

fn fixture() -> FleetFile {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../dashboard/evals/agent/machines.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn scenario(value: Value) -> Scenario {
    serde_json::from_value(value).unwrap()
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
            json!({"faults": [{"fault": "disconnect_after", "machine": "web-9", "command": "ensure_container:stopped"}]})
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
        let socket = socket_of(sockets.path(), "web-3");
        let channel = Endpoint::from_static("http://[::]:50051")
            .connect_with_connector(tower::service_fn(move |_| {
                let socket = socket.clone();
                async move { UnixStream::connect(socket).await.map(TokioIo::new) }
            }))
            .await
            .unwrap();
        let request = op::DescribeContract::into_request(DescribeContractRequest {})
            .encode()
            .unwrap();
        MachineRpcClient::new(channel)
            .describe_contract(request)
            .await
            .unwrap();
    });
    writeln!(stdin, r#"{{"offline":"web-3"}}"#).unwrap();
    assert_eq!(read(), json!({"ok": true}));
    assert!(!web_3.exists());

    drop(stdin);
    let (exited, exit) = std::sync::mpsc::channel();
    std::thread::spawn(move || exited.send(child.wait()));
    exit.recv_timeout(Duration::from_secs(10))
        .expect("the bin outlived its stdin")
        .unwrap();
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
