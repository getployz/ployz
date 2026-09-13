//! Façade tests for Cloud session Machine removal with named Data Loss.

use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

use ployz::sdk;
use ployz_core::{
    ContractDescription, DataLoss, DockerVolumeId, DockerVolumeName, MachineId, MachineObservation,
    RpcErrorCode, UnconfirmedDataLoss,
};
use tokio::time::timeout;

use super::support::{DiscoveryService, confirmation, connected_client, machine};
use super::support::{docker_volume, machine_named};
use super::unix_session::{self, UnixSession};

#[tokio::test]
async fn remove_machine_destroys_a_peer_after_named_data_loss_confirmation() {
    let (client, worker, empty, service, _session, _machine) = removal_session().await;
    let observed = client
        .data_loss_if_machine_removed(worker.name.as_str())
        .await
        .unwrap();
    assert_eq!(observed.data_loss.len(), 2);
    let reviewed_confirmation = confirmation(observed.data_loss);

    let removed = client
        .remove_machine(worker.name.as_str(), &reviewed_confirmation)
        .await
        .unwrap();
    assert!(removed.reset_warning.is_none());
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[worker.id]
    );
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.id]
    );

    let empty_confirmation = confirmation(Vec::<DataLoss>::new());
    let none = client
        .remove_machine(empty.name.as_str(), &empty_confirmation)
        .await
        .unwrap();
    assert!(none.reset_warning.is_none());
}

#[tokio::test]
async fn remove_machine_fails_when_fresh_data_loss_is_unconfirmed() {
    let (client, worker, _empty, service, _session, _machine) = removal_session().await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client
        .remove_machine(worker.name.as_str(), &confirmation)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(error.message.contains("data"));
    assert!(error.message.contains("logs"));
    let missing: UnconfirmedDataLoss = serde_json::from_value(error.details).unwrap();
    assert_eq!(
        missing.missing,
        [volume(worker.id, "data"), volume(worker.id, "logs"),]
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn remove_machine_ignores_confirmed_names_that_no_longer_exist() {
    let (client, worker, _empty, service, _session, _machine) = removal_session().await;
    let confirmation = confirmation([
        volume(worker.id, "data"),
        volume(worker.id, "logs"),
        volume(worker.id, "gone"),
    ]);

    client
        .remove_machine(worker.id.as_str(), &confirmation)
        .await
        .unwrap();
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[worker.id]
    );
}

#[tokio::test]
async fn remove_machine_refuses_the_current_entry_while_another_is_visible() {
    let (client, _worker, _empty, service, _session, _machine) = removal_session().await;
    let entry = client.about().await.unwrap().machine_id;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client
        .remove_machine(entry.as_str(), &confirmation)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        error.message.contains(
            "the current entry Machine cannot be removed while another Machine is visible"
        )
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
}

#[tokio::test]
async fn remove_machine_reports_a_failed_reset_instead_of_swallowing_it() {
    let (description, worker, _empty, service) = removal_cluster();
    *service.reset_warning.lock().unwrap() = Some("replicated delete failed".into());
    let session = UnixSession::start().await;
    let spawned = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    let confirmation = confirmation([
        volume(worker.machine.id, "data"),
        volume(worker.machine.id, "logs"),
    ]);

    let removed = client
        .remove_machine(worker.machine.name.as_str(), &confirmation)
        .await
        .unwrap();
    assert_eq!(
        removed.reset_warning.as_deref(),
        Some("replicated delete failed")
    );
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    drop(spawned);
}

#[tokio::test]
async fn remove_machine_refuses_the_last_cloud_paired_machine_before_mutation() {
    let (description, entry, service) = last_machine_cluster();
    service.cloud_paired.store(true, Ordering::SeqCst);
    let session = UnixSession::start().await;
    let spawned = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client
        .remove_machine(entry.machine.name.as_str(), &confirmation)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        error.message.contains("tear down this Cluster from Cloud"),
        "{}",
        error.message
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
    drop(spawned);
}

#[tokio::test]
async fn remove_machine_refuses_the_last_machine_with_stored_cloud_pairing() {
    let (_description, entry, service) = last_machine_cluster();
    service.cloud_paired.store(true, Ordering::SeqCst);
    let (mut client, server, _) = connected_client(service.clone()).await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client
        .remove_machine(
            &ployz_core::MachineTarget::from(&entry.machine.id),
            &confirmation,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        error.message.contains("tear down this Cluster from Cloud"),
        "{}",
        error.message
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn remove_machine_membership_refuses_the_last_machine_with_stored_cloud_pairing() {
    let (_description, entry, service) = last_machine_cluster();
    service.cloud_paired.store(true, Ordering::SeqCst);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&entry.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    assert!(
        error.message.contains("tear down this Cluster from Cloud"),
        "{}",
        error.message
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn remove_machine_membership_removes_the_final_unpaired_machine() {
    let (_description, entry, service) = last_machine_cluster();
    let (mut client, server, _) = connected_client(service.clone()).await;

    client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&entry.machine.id))
        .await
        .unwrap();
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[entry.machine.id]
    );
    server.abort();
}

#[tokio::test]
async fn remove_machine_removes_the_final_unpaired_machine() {
    let (_description, entry, service) = last_machine_cluster();
    let (mut client, server, _) = connected_client(service.clone()).await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let removed = client
        .remove_machine(
            &ployz_core::MachineTarget::from(&entry.machine.id),
            &confirmation,
        )
        .await
        .unwrap();
    assert!(removed.reset_warning.is_none());
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[entry.machine.id]
    );
    server.abort();
}

fn last_machine_cluster() -> (ContractDescription, MachineObservation, DiscoveryService) {
    let description = super::sdk::advertised_description();
    let entry = machine_named(&description.machine_id, "entry");
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![entry.clone()];
    *service.listed_volumes.lock().unwrap() = BTreeMap::from([(entry.machine.id, vec![])]);
    (description, entry, service)
}

#[tokio::test]
async fn node_remove_machine_covers_volumes_and_unconfirmed_missing_names() {
    let (description, worker, empty, service) = removal_cluster();
    let session = UnixSession::start().await;
    let _machine = session.spawn_machine(description.machine_id, service).await;
    session
        .assert_sdk_script(
            "node_remove_machine.js",
            description.machine_id,
            &[
                ("PLOYZ_WORKER_MACHINE", worker.machine.name.as_str()),
                ("PLOYZ_EMPTY_MACHINE", empty.machine.name.as_str()),
            ],
        )
        .await;
}

async fn removal_session() -> (
    sdk::Session,
    ployz_core::Machine,
    ployz_core::Machine,
    DiscoveryService,
    UnixSession,
    super::unix_session::FakeMachine,
) {
    let (description, worker, empty, service) = removal_cluster();
    let session = UnixSession::start().await;
    let spawned = session
        .spawn_machine(description.machine_id, service.clone())
        .await;
    let client = timeout(
        Duration::from_secs(5),
        unix_session::connect(&session.directory, description.machine_id.as_str()),
    )
    .await
    .expect("connect must not hang")
    .unwrap();
    (
        client,
        worker.machine,
        empty.machine,
        service,
        session,
        spawned,
    )
}

fn removal_cluster() -> (
    ContractDescription,
    MachineObservation,
    MachineObservation,
    DiscoveryService,
) {
    let description = super::sdk::advertised_description();
    let entry = machine_named(&description.machine_id, "entry");
    let worker = machine('c', "worker");
    let empty = machine('e', "empty");
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![entry, worker.clone(), empty.clone()];
    *service.listed_volumes.lock().unwrap() = BTreeMap::from([
        (
            worker.machine.id,
            vec![
                docker_volume(worker.machine.id, "data"),
                docker_volume(worker.machine.id, "logs"),
            ],
        ),
        (empty.machine.id, vec![]),
        (description.machine_id, vec![]),
    ]);
    (description, worker, empty, service)
}

fn volume(machine_id: MachineId, name: &str) -> DataLoss {
    DataLoss::DockerVolume {
        id: DockerVolumeId {
            machine_id,
            name: DockerVolumeName::parse(name).unwrap(),
        },
    }
}
