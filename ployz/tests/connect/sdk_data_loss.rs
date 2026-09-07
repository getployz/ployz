//! Façade tests for Cloud session Data Loss reads on Machine removal.

use std::{collections::BTreeMap, time::Duration};

use ployz::sdk;
use ployz_core::{
    ContractDescription, DataLoss, DockerVolumeId, DockerVolumeName, MachineObservation,
    MembershipObservation, RpcErrorCode,
};
use tokio::time::timeout;

use super::relay::{self, RelaySession};
use super::support::{DiscoveryService, machine};
use super::support::{docker_volume as volume, machine_named};

#[tokio::test]
async fn data_loss_if_machine_removed_lists_volumes_and_empty_without_mutating() {
    let (client, loaded, empty, _session, _machine) = session_with_two_machines().await;

    let with_volumes = client
        .data_loss_if_machine_removed(loaded.name.as_str())
        .await
        .unwrap();
    assert_eq!(
        with_volumes.data_loss,
        [
            DataLoss::DockerVolume {
                id: DockerVolumeId {
                    machine_id: loaded.id,
                    name: DockerVolumeName::parse("data").unwrap(),
                }
            },
            DataLoss::DockerVolume {
                id: DockerVolumeId {
                    machine_id: loaded.id,
                    name: DockerVolumeName::parse("logs").unwrap(),
                }
            },
        ]
    );

    let none = client
        .data_loss_if_machine_removed(empty.name.as_str())
        .await
        .unwrap();
    assert_eq!(none.data_loss, Vec::<DataLoss>::new());

    let again = client
        .data_loss_if_machine_removed(loaded.id.as_str())
        .await
        .unwrap();
    assert_eq!(again, with_volumes);
    assert!(
        client.about().await.is_ok(),
        "the read must leave the session usable"
    );
}

#[tokio::test]
async fn data_loss_if_machine_removed_is_observer_relative() {
    let (client, _loaded, _empty, _session, _machine) = session_with_two_machines().await;

    let error = client
        .data_loss_if_machine_removed("missing")
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert!(error.message.contains("was not found"));

    let invalid = client.data_loss_if_machine_removed("*").await.unwrap_err();
    assert_eq!(invalid.code, RpcErrorCode::InvalidArgument);
}

#[tokio::test]
async fn failed_volume_listing_is_not_empty_data_loss() {
    let description = super::sdk::advertised_description();
    let failed = machine('b', "broken");
    let machine_id = failed.machine.id;
    let session = RelaySession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![failed];
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = timeout(
        Duration::from_secs(5),
        sdk::connect(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            description.machine_id.as_str(),
        ),
    )
    .await
    .expect("connect must not hang")
    .unwrap();

    let error = client
        .data_loss_if_machine_removed("broken")
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        format!("Machine {machine_id}: target unavailable")
    );
}

#[tokio::test]
async fn omitted_volume_listing_is_not_empty_data_loss() {
    let description = super::sdk::advertised_description();
    let mut down = machine('c', "gone");
    down.membership = MembershipObservation::Down;
    let machine_id = down.machine.id;
    let session = RelaySession::start().await;
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![down];
    let _machine = session.spawn_machine(description.machine_id, service).await;
    let client = timeout(
        Duration::from_secs(5),
        sdk::connect(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            description.machine_id.as_str(),
        ),
    )
    .await
    .expect("connect must not hang")
    .unwrap();

    let error = client
        .data_loss_if_machine_removed("gone")
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        format!("Machine {machine_id} did not respond")
    );
}

#[tokio::test]
async fn node_data_loss_reads_a_machine_with_volumes_and_one_with_none() {
    let (description, loaded, empty, service) = two_machine_cluster();
    let session = RelaySession::start().await;
    let _machine = session.spawn_machine(description.machine_id, service).await;
    session
        .assert_sdk_script(
            "node_data_loss.js",
            description.machine_id,
            &[
                ("PLOYZ_LOADED_MACHINE", "loaded"),
                ("PLOYZ_LOADED_MACHINE_ID", loaded.machine.id.as_str()),
                ("PLOYZ_EMPTY_MACHINE", empty.machine.name.as_str()),
            ],
        )
        .await;
}

async fn session_with_two_machines() -> (
    sdk::Session,
    ployz_core::Machine,
    ployz_core::Machine,
    RelaySession,
    super::relay::FakeMachine,
) {
    let (description, loaded, empty, service) = two_machine_cluster();
    let session = RelaySession::start().await;
    let spawned = session.spawn_machine(description.machine_id, service).await;
    let client = timeout(
        Duration::from_secs(5),
        sdk::connect(
            &session.url,
            relay::DIAL,
            relay::PAIRING,
            description.machine_id.as_str(),
        ),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    (client, loaded.machine, empty.machine, session, spawned)
}

fn two_machine_cluster() -> (
    ContractDescription,
    MachineObservation,
    MachineObservation,
    DiscoveryService,
) {
    let description = super::sdk::advertised_description();
    let loaded = machine_named(&description.machine_id, "loaded");
    let empty = machine('c', "empty");
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![loaded.clone(), empty.clone()];
    *service.listed_volumes.lock().unwrap() = BTreeMap::from([
        (
            loaded.machine.id,
            vec![
                volume(loaded.machine.id, "data"),
                volume(loaded.machine.id, "logs"),
            ],
        ),
        (empty.machine.id, vec![]),
    ]);
    (description, loaded, empty, service)
}
