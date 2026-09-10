//! Daemon version synchronization before the Cloud enrollment exchange.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use clap::ArgMatches;
use ployz_core::{CloudPairing, PairingCredential};
use serde_json::json;

use super::harness::{
    EnrollListen, JoinDaemon, PAIRING, TOKEN, registration, serve_local_machine, serve_machine,
};

// Run successful local enrollment with an isolated PATH for capability export.
fn with_export_helper(test: &str) -> bool {
    if std::env::var_os("PLOYZ_EXPORT_FIXTURE").is_some() {
        return false;
    }
    let cli = super::harness::cli();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("daemon_sync::{test}"), "--nocapture"])
        .envs(
            cli.as_std()
                .get_envs()
                .filter_map(|(key, value)| value.map(|value| (key, value))),
        )
        .env("PLOYZ_EXPORT_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

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
) -> impl Fn(ployz_core::StorageChoice) -> std::future::Ready<Result<(), ployz::handlers::Error>> {
    move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        std::future::ready(match outcome {
            InstallOutcome::UpdateDaemon => {
                daemon.set_daemon_version(env!("CARGO_PKG_VERSION"));
                Ok(())
            }
            InstallOutcome::LeaveStale => Ok(()),
            InstallOutcome::Fail(message) => Err(ployz::handlers::Error::usage(message)),
        })
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
            "--accepts-ingress=false",
            "--no-dns",
            "--yes",
        ])
        .unwrap()
}

async fn run_enroll(
    matches: ArgMatches,
    install: impl Fn(
        ployz_core::StorageChoice,
    ) -> std::future::Ready<Result<(), ployz::handlers::Error>>
    + Send
    + 'static,
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
    enroll_locally_with_storage(daemon_version, outcome, "none").await
}

async fn enroll_locally_with_storage(
    daemon_version: &str,
    outcome: InstallOutcome,
    storage: &str,
) -> (
    Result<(), ployz::handlers::Error>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let mut registration = registration();
    registration.assigned_machine.accepts_ingress = false;
    let pairing = CloudPairing::new(PairingCredential::parse(PAIRING).unwrap());
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": storage,
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
async fn zfs_preparation_reconnects_after_restarting_a_matching_daemon() {
    if with_export_helper("zfs_preparation_reconnects_after_restarting_a_matching_daemon") {
        return;
    }
    let (result, calls, connections) = enroll_locally_with_storage(
        env!("CARGO_PKG_VERSION"),
        InstallOutcome::UpdateDaemon,
        "zfs",
    )
    .await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        connections.load(Ordering::SeqCst) >= 2,
        "enrollment must reconnect after storage preparation restarts the daemon"
    );
}

#[tokio::test]
async fn matching_daemon_only_prepares_the_capability_helper() {
    if with_export_helper("matching_daemon_only_prepares_the_capability_helper") {
        return;
    }
    let (result, calls, _) =
        enroll_locally(env!("CARGO_PKG_VERSION"), InstallOutcome::UpdateDaemon).await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mismatched_daemon_is_reinstalled_without_preparing_storage() {
    if with_export_helper("mismatched_daemon_is_reinstalled_without_preparing_storage") {
        return;
    }
    let (result, calls, connections) =
        enroll_locally("0.0.0-old", InstallOutcome::UpdateDaemon).await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
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
