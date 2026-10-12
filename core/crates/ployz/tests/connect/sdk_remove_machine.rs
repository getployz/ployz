//! Façade tests for Cloud session Machine removal with named Data Loss.

use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

use ployz::sdk;
use ployz_core::{
    ContractDescription, DataLoss, DockerVolumeId, DockerVolumeName, MachineId, MachineObservation,
    MembershipObservation, RpcError, RpcErrorCode, UnconfirmedDataLoss,
};
use serde_json::{Value, json};
use tokio::time::timeout;

use ployz::connect::Remover;

use super::support::{DiscoveryService, confirmation, connected_client, machine};
use super::support::{docker_volume, empty_storage, machine_named};
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
async fn sending_a_removal_again_skips_a_machine_already_forgotten_by_id() {
    let (client, worker, _empty, service, _session, _machine) = removal_session().await;
    let confirmed = confirmation(
        client
            .data_loss_if_machine_removed(worker.name.as_str())
            .await
            .unwrap()
            .data_loss,
    );
    client
        .remove_machine(worker.name.as_str(), &confirmed)
        .await
        .unwrap();

    let again = client
        .remove_machine(worker.id.as_str(), &confirmed)
        .await
        .unwrap();
    assert_eq!(
        again.reset_warning.as_deref(),
        Some("its reset couldn't be confirmed: it was already gone from the Cluster")
    );
    client
        .remove_machine_membership(worker.id.as_str())
        .await
        .unwrap();
    // Neither send reached the Machine: one Leave and one Forget, from the first removal.
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[worker.id]
    );
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.id]
    );
    // A name that no longer resolves isn't taken as removed.
    let error = client
        .remove_machine(worker.name.as_str(), &confirmed)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
}

#[tokio::test]
async fn a_removal_sent_again_after_a_failed_read_runs_every_guard_again() {
    let (_description, _entry, mut service) = last_machine_cluster();
    let worker = machine('b', "worker");
    service.machines.push(worker.clone());
    let (mut client, server, _) = connected_client(service.clone()).await;
    let target = ployz_core::MachineTarget::from(&worker.machine.id);
    let partial = confirmation([volume(worker.machine.id, "data")]);

    // The Machine doesn't answer its Data Loss read, twice: nothing leaves either time.
    for _ in 0..2 {
        let error = client
            .remove_machine(&target, &partial, Remover::Cloud)
            .await
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Unavailable, "{}", error.message);
    }
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());

    // It answers the next send with Data Loss the confirmation doesn't cover.
    service.listed_volumes.lock().unwrap().insert(
        worker.machine.id,
        vec![
            docker_volume(worker.machine.id, "data"),
            docker_volume(worker.machine.id, "logs"),
        ],
    );
    let error = client
        .remove_machine(&target, &partial, Remover::Cloud)
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        RpcErrorCode::InvalidArgument,
        "{}",
        error.message
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());

    let full = confirmation([
        volume(worker.machine.id, "data"),
        volume(worker.machine.id, "logs"),
    ]);
    client
        .remove_machine(&target, &full, Remover::Cloud)
        .await
        .unwrap();
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    server.abort();
}

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
async fn an_unreachable_leave_forgets_and_a_resend_still_cannot_confirm_the_reset() {
    let (_description, worker, _empty, mut service) = removal_cluster();
    service.leave_unavailable = true;
    let (mut client, server, _) = connected_client(service.clone()).await;
    let target = ployz_core::MachineTarget::from(&worker.machine.id);
    let confirmed = confirmation([
        volume(worker.machine.id, "data"),
        volume(worker.machine.id, "logs"),
    ]);

    let first = client
        .remove_machine(&target, &confirmed, Remover::Cloud)
        .await
        .unwrap();
    assert!(
        first
            .reset_warning
            .as_deref()
            .is_some_and(|warning| warning.starts_with("target is unreachable")),
        "{first:?}"
    );
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );

    let again = client
        .remove_machine(&target, &confirmed, Remover::Cloud)
        .await
        .unwrap();
    assert_eq!(
        again.reset_warning.as_deref(),
        Some("its reset couldn't be confirmed: it was already gone from the Cluster")
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    server.abort();
}

#[tokio::test]
async fn a_leave_the_entry_no_longer_routes_still_forgets_without_confirming_the_reset() {
    let (_description, worker, _empty, mut service) = removal_cluster();
    service.leave_error = Some(RpcError {
        code: RpcErrorCode::NotFound,
        message: "unknown Machine".into(),
        details: Value::Null,
        cause: Vec::new(),
    });
    let (mut client, server, _) = connected_client(service.clone()).await;
    let confirmed = confirmation([
        volume(worker.machine.id, "data"),
        volume(worker.machine.id, "logs"),
    ]);

    let removed = client
        .remove_machine(
            &ployz_core::MachineTarget::from(&worker.machine.id),
            &confirmed,
            Remover::Cloud,
        )
        .await
        .unwrap();
    assert_eq!(
        removed.reset_warning.as_deref(),
        Some("its reset couldn't be confirmed: it was already gone from the Cluster")
    );
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    server.abort();
}

#[tokio::test]
async fn a_forget_that_fails_after_left_carries_the_leave_reply() {
    let (_description, worker, _empty, service) = removal_cluster();
    *service.reset_warning.lock().unwrap() = Some("Docker reset failed".into());
    service.forget_failures.store(1, Ordering::SeqCst);
    let (mut client, server, _) = connected_client(service.clone()).await;
    let target = ployz_core::MachineTarget::from(&worker.machine.id);
    let confirmed = confirmation([
        volume(worker.machine.id, "data"),
        volume(worker.machine.id, "logs"),
    ]);

    let error = client
        .remove_machine(&target, &confirmed, Remover::Cloud)
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable, "{}", error.message);
    assert_eq!(
        error.details,
        json!({ "left": true, "reset_warning": "Docker reset failed" })
    );
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

/// The reset target's own row delete failed and so did the entry's Forget. Once the target
/// reads Down, a full removal can't read it, and the membership removal still forgets it.
#[tokio::test]
async fn a_reset_target_that_forget_missed_is_forgotten_by_membership_removal() {
    let (_description, worker, _empty, service) = removal_cluster();
    service.forget_failures.store(1, Ordering::SeqCst);
    let (mut client, server, _) = connected_client(service.clone()).await;
    let target = ployz_core::MachineTarget::from(&worker.machine.id);
    let confirmed = confirmation([
        volume(worker.machine.id, "data"),
        volume(worker.machine.id, "logs"),
    ]);
    client
        .remove_machine(&target, &confirmed, Remover::Cloud)
        .await
        .unwrap_err();
    server.abort();

    let mut after_reset = service.clone();
    for observed in &mut after_reset.machines {
        if observed.machine.id == worker.machine.id {
            observed.membership = MembershipObservation::Down;
        }
    }
    let (mut client, server, _) = connected_client(after_reset).await;
    let unread = client
        .remove_machine(&target, &confirmed, Remover::Cloud)
        .await
        .unwrap_err();
    assert_eq!(unread.code, RpcErrorCode::Unavailable, "{}", unread.message);
    client.remove_machine_membership(&target).await.unwrap();
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    assert_eq!(
        service.removed_machines.lock().unwrap().as_slice(),
        &[worker.machine.id]
    );
    server.abort();
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
async fn cloud_removes_the_last_machine_it_holds() {
    let (description, entry, service) = last_machine_cluster();
    // Cloud's own hold: its key and a device key it provisioned. The reset takes both.
    hold_keys(&service, &["cloud", "cli-0123456789abcdef0123456789ab"]);
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

    let removed = client
        .remove_machine(entry.machine.name.as_str(), &confirmation)
        .await
        .unwrap();
    assert!(removed.reset_warning.is_none());
    assert_eq!(
        service.reset_machines.lock().unwrap().as_slice(),
        &[entry.machine.id]
    );
    drop(spawned);
}

#[tokio::test]
async fn remove_machine_refuses_the_last_machine_another_management_client_holds() {
    let (_description, entry, service) = last_machine_cluster();
    hold_keys(
        &service,
        &["cloud", "cli-0123456789abcdef0123456789ab", "ops"],
    );
    let (mut client, server, _) = connected_client(service.clone()).await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let error = client
        .remove_machine(
            &ployz_core::MachineTarget::from(&entry.machine.id),
            &confirmation,
            Remover::Operator,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    // Only the holder Cloud doesn't release is named.
    assert!(
        error
            .message
            .ends_with("Disconnect `ops` from this Machine first."),
        "{}",
        error.message
    );
    assert!(service.reset_machines.lock().unwrap().is_empty());
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

    let error = client
        .remove_machine(
            &target,
            &confirmation(Vec::<DataLoss>::new()),
            Remover::Operator,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    let error = client.remove_machine_membership(&target).await.unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert!(service.reset_machines.lock().unwrap().is_empty());
    assert!(service.removed_machines.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn unreadable_holders_of_the_last_machine_refuse_its_removal() {
    let (_description, entry, mut service) = last_machine_cluster();
    service.inspect_fails = true;
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine(
            &ployz_core::MachineTarget::from(&entry.machine.id),
            &confirmation(Vec::<DataLoss>::new()),
            Remover::Operator,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable, "{}", error.message);
    assert!(service.reset_machines.lock().unwrap().is_empty());
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

#[tokio::test]
async fn remove_machine_removes_the_final_unpaired_machine() {
    let (_description, entry, service) = last_machine_cluster();
    let (mut client, server, _) = connected_client(service.clone()).await;
    let confirmation = confirmation(Vec::<DataLoss>::new());

    let removed = client
        .remove_machine(
            &ployz_core::MachineTarget::from(&entry.machine.id),
            &confirmation,
            Remover::Operator,
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

#[tokio::test]
async fn remove_machine_membership_refuses_a_server_that_answers_holding_copies() {
    let (_description, peer, service) = cluster_with_a_held_peer(MembershipObservation::Up);
    let (mut client, server, _) = connected_client(service.clone()).await;

    let error = client
        .remove_machine_membership(&ployz_core::MachineTarget::from(&peer.machine.id))
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(
        error.message,
        "Server peer answers and holds copies of Volumes data, logs; removing it without a reset \
         leaves them behind. No changes made. Drop --no-reset so the reset demotes them."
    );
    assert!(service.removed_machines.lock().unwrap().is_empty());
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
        "Cannot read the Volume copies Server peer holds. No changes made."
    );
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
        volumes: BTreeMap::from([(docker("app-prod_data"), bytes)]),
        copies: BTreeMap::from([(
            docker("app-prod_logs"),
            ployz_core::ProvisionedCopy {
                role: ployz_core::CopyRole::Slot,
                maximum_bytes: bytes,
                used_bytes: 0,
            },
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
