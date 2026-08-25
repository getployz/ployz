//! Daemon version synchronization before the Cloud enrollment exchange.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use clap::ArgMatches;
use ployz_core::{CloudPairing, PairingCredential};
use serde_json::json;

use super::harness::{
    EnrollListen, JoinDaemon, PAIRING, RelayListen, TOKEN, registration, serve_local_machine,
    serve_machine,
};

#[derive(Clone, Copy)]
enum InstallOutcome {
    UpdateDaemon,
    LeaveStale,
    Fail(&'static str),
}

#[derive(Clone)]
struct RecordingInstaller {
    daemon: JoinDaemon,
    outcome: InstallOutcome,
    calls: Arc<AtomicUsize>,
}

impl RecordingInstaller {
    fn new(daemon: JoinDaemon, outcome: InstallOutcome) -> Self {
        Self {
            daemon,
            outcome,
            calls: Arc::default(),
        }
    }
}

impl ployz::handlers::EnrollInstaller for RecordingInstaller {
    fn install_cli_daemon_without_storage(&self) -> Result<(), ployz::handlers::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.outcome {
            InstallOutcome::UpdateDaemon => {
                self.daemon.set_daemon_version(env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            InstallOutcome::LeaveStale => Ok(()),
            InstallOutcome::Fail(message) => Err(ployz::handlers::Error::usage(message)),
        }
    }
}

fn enroll_matches(connect: &str, cloud_url: &str) -> ArgMatches {
    ployz::cli::command()
        .try_get_matches_from([
            "ployz",
            "--connect",
            connect,
            "cloud",
            "enroll",
            TOKEN,
            "--cloud-url",
            cloud_url,
            "--name",
            "joiner",
            "--no-ingress",
            "--no-dns",
            "--yes",
        ])
        .unwrap()
}

async fn run_enroll(
    matches: ArgMatches,
    installer: RecordingInstaller,
) -> Result<(), ployz::handlers::Error> {
    tokio::task::spawn_blocking(move || {
        ployz::handlers::cloud_enroll_with_installer(&matches, &installer)
    })
    .await
    .unwrap()
}

async fn enroll_locally(
    daemon_version: &str,
    outcome: InstallOutcome,
) -> (
    Result<(), ployz::handlers::Error>,
    RecordingInstaller,
    Arc<AtomicUsize>,
) {
    let registration = registration();
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
    let daemon = JoinDaemon::new(registration);
    daemon.set_daemon_version(daemon_version);
    let (connect, socket, connections) = serve_local_machine(daemon.clone()).await;
    let installer = RecordingInstaller::new(daemon.clone(), outcome);
    let result = run_enroll(enroll_matches(&connect, &enroll.url), installer.clone()).await;
    let _ = std::fs::remove_file(socket);
    (result, installer, connections)
}

#[tokio::test]
async fn matching_daemon_does_not_run_the_installer() {
    let (result, installer, _) =
        enroll_locally(env!("CARGO_PKG_VERSION"), InstallOutcome::UpdateDaemon).await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(installer.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mismatched_daemon_is_reinstalled_without_preparing_storage() {
    let (result, installer, connections) =
        enroll_locally("0.0.0-old", InstallOutcome::UpdateDaemon).await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(installer.calls.load(Ordering::SeqCst), 1);
    assert!(
        connections.load(Ordering::SeqCst) >= 2,
        "enrollment must reconnect after the installer restarts the daemon"
    );
}

#[tokio::test]
async fn installer_failure_is_returned_before_enrollment() {
    let (result, installer, _) =
        enroll_locally("0.0.0-old", InstallOutcome::Fail("installer failed")).await;

    assert_eq!(result.unwrap_err().to_string(), "installer failed");
    assert_eq!(installer.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stale_daemon_after_installation_is_rejected() {
    let (result, _, _) = enroll_locally("0.0.0-old", InstallOutcome::LeaveStale).await;

    assert_eq!(
        result.unwrap_err().to_string(),
        format!(
            "daemon version remained 0.0.0-old after installing CLI version {}",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[tokio::test]
async fn remote_mismatch_is_rejected_without_mutating_the_local_machine() {
    let registration = registration();
    let daemon = JoinDaemon::new(registration);
    daemon.set_daemon_version("0.0.0-old");
    let address = serve_machine(daemon.clone()).await;
    let connect = format!("tcp://{address}");
    let installer = RecordingInstaller::new(daemon, InstallOutcome::UpdateDaemon);
    let result = run_enroll(
        enroll_matches(&connect, "http://127.0.0.1:9"),
        installer.clone(),
    )
    .await;

    assert_eq!(
        result.unwrap_err().to_string(),
        format!(
            "daemon version synchronization requires running ployz cloud enroll on the Machine itself; connected through {connect}"
        )
    );
    assert_eq!(installer.calls.load(Ordering::SeqCst), 0);
}
