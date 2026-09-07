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

/// A stand-in installer that counts calls into `calls` instead of provisioning.
fn recording_installer(
    daemon: JoinDaemon,
    outcome: InstallOutcome,
    calls: Arc<AtomicUsize>,
) -> impl Fn() -> Result<(), ployz::handlers::Error> {
    move || {
        calls.fetch_add(1, Ordering::SeqCst);
        match outcome {
            InstallOutcome::UpdateDaemon => {
                daemon.set_daemon_version(env!("CARGO_PKG_VERSION"));
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
    install: impl Fn() -> Result<(), ployz::handlers::Error> + Send + 'static,
) -> Result<(), ployz::handlers::Error> {
    tokio::task::spawn_blocking(move || {
        ployz::handlers::cloud_enroll_with_installer(&matches, &install)
    })
    .await
    .unwrap()
}

async fn enroll_locally(
    daemon_version: &str,
    outcome: InstallOutcome,
) -> (
    Result<(), ployz::handlers::Error>,
    Arc<AtomicUsize>,
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
    let calls = Arc::<AtomicUsize>::default();
    let installer = recording_installer(daemon.clone(), outcome, Arc::clone(&calls));
    let result = run_enroll(enroll_matches(&connect, &enroll.url), installer).await;
    let _ = std::fs::remove_file(socket);
    (result, calls, connections)
}

#[tokio::test]
async fn matching_daemon_does_not_run_the_installer() {
    let (result, calls, _) =
        enroll_locally(env!("CARGO_PKG_VERSION"), InstallOutcome::UpdateDaemon).await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mismatched_daemon_is_reinstalled_without_preparing_storage() {
    let (result, calls, connections) =
        enroll_locally("0.0.0-old", InstallOutcome::UpdateDaemon).await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        connections.load(Ordering::SeqCst) >= 2,
        "enrollment must reconnect after the installer restarts the daemon"
    );
}

#[tokio::test]
async fn installer_failure_is_returned_before_enrollment() {
    let (result, calls, _) =
        enroll_locally("0.0.0-old", InstallOutcome::Fail("installer failed")).await;

    assert_eq!(result.unwrap_err().to_string(), "installer failed");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let calls = Arc::<AtomicUsize>::default();
    let installer = recording_installer(daemon, InstallOutcome::UpdateDaemon, Arc::clone(&calls));
    let result = run_enroll(enroll_matches(&connect, "http://127.0.0.1:9"), installer).await;

    assert_eq!(
        result.unwrap_err().to_string(),
        format!(
            "daemon version synchronization requires running ployz cloud enroll on the Machine itself; connected through {connect}"
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
