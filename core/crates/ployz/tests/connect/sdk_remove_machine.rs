//! Façade tests for Cloud session Machine removal: a removed Server is forgotten.

use std::collections::BTreeMap;

use ployz_core::{
    ContractDescription, DockerVolumeName, MachineObservation, MembershipObservation, RpcErrorCode,
};

use super::support::{DiscoveryService, connected_client, machine};
use super::support::{empty_storage, machine_named};

#[tokio::test]
async fn a_membership_removal_sent_again_after_a_failed_storage_read_reads_it_again() {
    let (_description, peer, mut service) = cluster_with_a_held_peer(MembershipObservation::Up);
    service.storage_capacity = None;
    let (mut client, server, _) = connected_client(service.clone()).await;
    let target = ployz_core::MachineTarget::from(&peer.machine.id);

    for _ in 0..2 {
        let error = client.remove_machine_membership(&target).await.unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Unavailable, "{}", error.message);
    }
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn last_machine_refusal_names_non_cloud_holders_without_cloud_teardown() {
    let (_description, entry, service) = last_machine_cluster();
    hold_keys(&service, &["cli", "ops"]);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&entry.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(
        error.message,
        "this is the last Machine in the Cluster and it is still managed by `cli` and `ops`; \
         removing it would leave `cli` and `ops` managing a Cluster that no longer exists. \
         No changes made. Disconnect `cli` and `ops` from this Machine first."
    );
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn only_cloud_removes_the_last_machine_it_holds() {
    let (_description, entry, service) = last_machine_cluster();
    hold_keys(&service, &["cloud"]);
    let (mut client, server, _) = connected_client(service.clone()).await;
    let target = ployz_core::MachineTarget::from(&entry.machine.id);

    let error = client.remove_machine_membership(&target).await.unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

/// Since every Cloud founding arms a `device-` First Key, that key is part of Cloud's
/// hold: the last Machine is left to Cloud's removal, not refused as someone else's.
#[tokio::test]
async fn a_founding_device_key_is_part_of_clouds_hold_on_the_last_machine() {
    let (_description, entry, service) = last_machine_cluster();
    hold_keys(&service, &["cloud", "cli-laptop", "device-jzvsicbxap"]);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&entry.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert!(
        error.message.contains("Cloud manages it"),
        "{}",
        error.message
    );
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn a_device_key_without_cloud_still_refuses_the_last_machine() {
    let (_description, entry, service) = last_machine_cluster();
    hold_keys(&service, &["device-jzvsicbxap"]);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&entry.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert!(
        error
            .message
            .contains("still managed by `device-jzvsicbxap`"),
        "{}",
        error.message
    );
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn unreadable_holders_of_the_last_machine_refuse_its_removal() {
    let (_description, entry, mut service) = last_machine_cluster();
    service.inspect_fails = true;
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&entry.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable, "{}", error.message);
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

fn hold_keys(service: &DiscoveryService, labels: &[&str]) {
    *service.management_clients.lock().unwrap() = labels
        .iter()
        .map(|label| ployz_core::ManagementClientLabel::parse(*label).unwrap())
        .collect();
}

#[tokio::test]
async fn remove_machine_membership_refuses_the_last_machine_with_a_management_client() {
    let (_description, entry, service) = last_machine_cluster();
    hold_keys(&service, &["ops"]);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&entry.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert!(error.message.contains("`ops`"), "{}", error.message);
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

/// A removed Server is forgotten: one that answers and holds Volume copies goes like any
/// other, its row deleted and its key refused, and nothing is sent to reset it.
#[tokio::test]
async fn remove_machine_membership_forgets_a_server_that_answers_holding_copies() {
    let (_description, peer, service) = cluster_with_a_held_peer(MembershipObservation::Up);
    let (mut client, server, _) = connected_client(service.clone()).await;

    client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&peer.machine.id))
        .await
        .unwrap();
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[peer.machine.id]
    );
    server.abort();
}

#[tokio::test]
async fn remove_machine_membership_refuses_a_server_whose_storage_cannot_be_read() {
    let (_description, peer, mut service) = cluster_with_a_held_peer(MembershipObservation::Up);
    service.storage_capacity = None;
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&peer.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        "Couldn't check whether Server peer answers. No changes made. Run the command again, \
         one Server at a time if you're removing several."
    );
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn remove_machine_membership_refuses_a_server_that_answered_before_going_quiet() {
    let (_description, peer, mut service) = cluster_with_a_held_peer(MembershipObservation::Up);
    service.storage_unavailable_for = 1;
    service.storage_hangs_after = Some(1);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&peer.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn remove_machine_membership_removes_a_server_that_does_not_answer() {
    let (_description, peer, service) = cluster_with_a_held_peer(MembershipObservation::Down);
    let (mut client, server, _) = connected_client(service.clone()).await;

    client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&peer.machine.id))
        .await
        .unwrap();
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[peer.machine.id]
    );
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn remove_machine_membership_forgets_an_up_server_that_never_replies() {
    let (_description, peer, mut service) = cluster_with_a_held_peer(MembershipObservation::Up);
    service.storage_hangs_after = Some(0);
    let (mut client, server, _) = connected_client(service.clone()).await;

    client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&peer.machine.id))
        .await
        .unwrap();
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[peer.machine.id]
    );
    server.abort();
}

/// An entry Machine and a peer `membership` reports, whose storage holds Volume `data`'s
/// writer and a slot of Volume `logs`.
fn cluster_with_a_held_peer(
    membership: MembershipObservation,
) -> (ContractDescription, MachineObservation, DiscoveryService) {
    let (description, entry, mut service) = last_machine_cluster();
    let mut peer = machine('b', "peer");
    peer.membership = membership;
    service.machines = vec![entry, peer.clone()];
    let bytes = ployz_core::ProvisionedVolumeMaximumBytes::new(
        std::num::NonZeroU64::new(ployz_core::STORAGE_GIB).unwrap(),
    );
    let docker = |name: &str| DockerVolumeName::parse(name).unwrap();
    service.storage_capacity = Some(ployz_core::StorageCapacity {
        backing: ployz_core::StorageBacking::Unallocated {
            host_total_bytes: 100 * ployz_core::STORAGE_GIB,
            host_available_bytes: 90 * ployz_core::STORAGE_GIB,
        },
        unmanaged_used_bytes: 0,
        slot_bound_bytes: 0,
        volumes: BTreeMap::from([(docker("app-prod_data"), bytes)]),
        copies: BTreeMap::from([(
            docker("app-prod_logs"),
            ployz_core::ProvisionedCopy::new(ployz_core::CopyPlace::Slot(None), bytes, 0),
        )]),
    });
    (description, peer, service)
}

fn last_machine_cluster() -> (ContractDescription, MachineObservation, DiscoveryService) {
    let description = super::sdk::advertised_description();
    let entry = machine_named(&description.machine_id, "entry");
    let mut service = DiscoveryService::new(description.clone());
    service.machines = vec![entry.clone()];
    *service.listed_volumes.lock().unwrap() = BTreeMap::from([(entry.machine.id, vec![])]);
    service.storage_capacity = Some(empty_storage());
    (description, entry, service)
}
