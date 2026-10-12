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
