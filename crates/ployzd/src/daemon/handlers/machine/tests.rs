use super::join::release_reserved_subnet;
use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use crate::daemon::ActiveMesh;
use crate::daemon::DaemonState;
use crate::daemon::handlers::RequestLane;
use crate::daemon::ssh::{TestSshEnvGuard, TestSshProgramGuard, test_ssh_env_lock};
use crate::mesh_state::bootstrap::{bootstrap_peers_path, load_bootstrap_peer_records};
use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
use ipnet::Ipv4Net;
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineAddOptions, MachineSelfTransition,
    MachineStorageAuthorityPeer, MachineStoragePromoteRequest, MeshSelfRecordPayload,
    StatusPayload,
};
use ployz_orchestrator::Mesh;
use ployz_orchestrator::mesh::driver::WireguardDriver;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_runtime_api::Identity;
use ployz_store_api::StoreDriver;
use ployz_store_api::{InviteStore, MachineMembershipStore};
use ployz_store_memory::{MemoryService, MemoryStore, StoreDriverMemoryExt as _};
use ployz_types::model::{
    MachineId, MachineLifecycle, MachineMembership, MachineStorageRole, MachineTopology,
    NetworkLifecycle, OverlayIp, PublicKey, RegionRole, StorageParticipation, StorageReplicaPolicy,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn machine_list_shows_disabled_explicitly() {
    let (state, store, _) = make_state(false).await;
    let disabled = test_machine_record(
        "peer-disabled",
        "10.210.1.0/24",
        MachineLifecycle::Standby,
        PublicKey([2; 32]),
    );
    store
        .upsert_self_machine(&disabled)
        .await
        .expect("upsert disabled peer");

    let response = state.handle_machine_list().await;
    assert!(response.is_ok());
    assert!(!response.message().contains("LIVENESS"));
    assert!(response.message().contains("peer-disabled"));
    assert!(response.message().contains("disabled"));
}

#[tokio::test]
async fn machine_list_shows_active_lifecycle() {
    let (state, store, _) = make_state(false).await;
    let active_peer = test_machine_record(
        "peer-active",
        "10.210.1.0/24",
        MachineLifecycle::Active,
        PublicKey([2; 32]),
    );
    store
        .upsert_self_machine(&active_peer)
        .await
        .expect("upsert active peer");

    let response = state.handle_machine_list().await;
    assert!(response.is_ok());
    assert!(response.message().contains("peer-active"));
    assert!(response.message().contains("active"));
}

#[tokio::test]
async fn machine_list_json_payload_contains_rows() {
    let (state, store, _) = make_state(false).await;
    let mut candidate = test_machine_record(
        "peer-candidate",
        "10.210.1.0/24",
        MachineLifecycle::Standby,
        PublicKey([2; 32]),
    );
    candidate.storage_role = StorageParticipation::Candidate.into();
    store
        .upsert_self_machine(&candidate)
        .await
        .expect("upsert candidate");
    let mut compute = test_machine_record(
        "peer-compute",
        "10.210.2.0/24",
        MachineLifecycle::Standby,
        PublicKey([3; 32]),
    );
    compute.storage_role = MachineStorageRole::Compute;
    store
        .upsert_self_machine(&compute)
        .await
        .expect("upsert compute");

    let response = state.handle_machine_list().await;
    let Some(DaemonPayload::MachineList(payload)) = response.payload() else {
        panic!("expected machine list payload");
    };
    assert_eq!(payload.rows.len(), 3);
    let founder = payload
        .rows
        .iter()
        .find(|row| row.id == "founder")
        .expect("founder row");
    assert_eq!(founder.id, "founder");
    assert_eq!(founder.region_role, "home_data");
    assert_eq!(
        founder.authority.role(),
        ployz_types::model::AuthorityNodeRole::AuthorityStorage {
            authority_id: ployz_types::model::AuthorityId::default_authority(),
        }
    );
    assert_eq!(
        founder.authority.loss_impact(),
        ployz_types::model::ControlPlaneLossImpact::StoredTruthLost
    );
    let candidate = payload
        .rows
        .iter()
        .find(|row| row.id == "peer-candidate")
        .expect("candidate row");
    assert_eq!(
        candidate.authority.role(),
        ployz_types::model::AuthorityNodeRole::StorageCandidate
    );
    assert_eq!(
        candidate.authority.data_bucket(),
        ployz_types::model::ControlPlaneDataBucket::StoredIntent
    );
    assert_eq!(
        candidate.authority.loss_impact(),
        ployz_types::model::ControlPlaneLossImpact::NoStoredTruthLost
    );
    assert_eq!(candidate.region_role, "home_data");
    let compute = payload
        .rows
        .iter()
        .find(|row| row.id == "peer-compute")
        .expect("compute row");
    assert_eq!(
        compute.authority.role(),
        ployz_types::model::AuthorityNodeRole::Compute
    );
    assert_eq!(
        compute.authority.data_bucket(),
        ployz_types::model::ControlPlaneDataBucket::LiveFacts
    );
    assert_eq!(
        compute.authority.loss_impact(),
        ployz_types::model::ControlPlaneLossImpact::NoStoredTruthLost
    );
    assert_eq!(compute.region_role, "home_data");
}

#[tokio::test]
async fn machine_rtt_does_not_fan_out_to_unreachable_peer() {
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("bind overlay listener");
    let unused_listener_port = listener.local_addr().expect("listener addr").port();
    drop(listener);
    let (mut state, store, _) = make_state_with_zfs_transfer_port(true, unused_listener_port).await;

    let mut peer = test_machine_record(
        "peer-1",
        "10.210.1.0/24",
        MachineLifecycle::Active,
        PublicKey([2; 32]),
    );
    peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    store.upsert_self_machine(&peer).await.expect("upsert peer");

    let response = state.handle_machine_rtt().await;

    assert!(response.is_ok());
    let Some(DaemonPayload::MachineRtt(payload)) = response.payload() else {
        panic!("expected machine rtt payload");
    };
    assert!(payload.rows.is_empty());
    assert!(payload.warnings.is_empty());
    assert_eq!(response.message(), "no rtt samples");

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_activates_joiner_lifecycle() {
    let _guard = test_ssh_env_lock().lock().await;
    let (mut state, store, network) = make_state(true).await;
    let mut stale_peer = test_machine_record(
        "stale-peer",
        "10.210.1.0/24",
        MachineLifecycle::Active,
        PublicKey([3; 32]),
    );
    stale_peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    store
        .upsert_self_machine(&stale_peer)
        .await
        .expect("upsert stale peer");

    let reserved = state
        .reserve_machine_subnet(&MachineId::new("join-target"))
        .await
        .expect("reserve expected subnet");
    let expected_subnet = reserved.subnet();
    release_reserved_subnet(reserved)
        .await
        .expect("release reserved subnet for re-acquire");

    let mut joiner_record = test_machine_record(
        "joiner-1",
        &expected_subnet.to_string(),
        MachineLifecycle::Active,
        PublicKey([4; 32]),
    );
    joiner_record.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    joiner_record.storage_role = StorageParticipation::Candidate.into();
    joiner_record.region_role = RegionRole::Compute;
    joiner_record.endpoints = vec!["203.0.113.10:51820".into()];

    let ssh_dir = unique_temp_dir("ployz-fake-ssh");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let self_record_response = serde_json::to_string(&DaemonResponse::success(
        "self-record",
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            record: joiner_record.clone(),
        })),
    ))
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _status_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_STATUS_RESPONSE",
        Some(status_response_for_record(&joiner_record).into()),
    );
    let _ready_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_READY_RESPONSE",
        Some(
            "{\"status\":\"success\",\"code\":\"OK\",\"message\":\"ready\",\"payload\":{\"kind\":\"mesh-ready\",\"ready\":true,\"phase\":\"running\",\"store_healthy\":true,\"sync_connected\":true,\"workload_subnet_present\":true}}".into(),
        ),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(response.is_ok(), "{}", response.message());
    assert!(response.message().contains("awaiting_self_publication: 1"));

    let joiner = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id.as_str() == "joiner-1")
        .expect("joiner should be in store after enable");
    assert_eq!(joiner.lifecycle, MachineLifecycle::Active);
    assert!(joiner.storage());
    assert_eq!(
        joiner.storage_participation(),
        StorageParticipation::Candidate
    );
    assert_eq!(joiner.region_role, RegionRole::Compute);
    assert!(
        network
            .current_peers()
            .into_iter()
            .any(|peer| peer.id().as_str() == "joiner-1")
    );
    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_requires_sync_connected_for_running_joiner() {
    let _guard = test_ssh_env_lock().lock().await;
    let (mut state, store, network) = make_state(true).await;
    state.machine_add_remote_ready_wait_policy = Some(super::types::RemoteReadyWaitPolicy::new(
        Duration::from_millis(50),
        Duration::from_millis(1),
        Duration::from_millis(20),
    ));

    let reserved = state
        .reserve_machine_subnet(&MachineId::new("join-target"))
        .await
        .expect("reserve expected subnet");
    let expected_subnet = reserved.subnet();
    release_reserved_subnet(reserved)
        .await
        .expect("release reserved subnet for re-acquire");

    let mut joiner_record = test_machine_record(
        "joiner-2",
        &expected_subnet.to_string(),
        MachineLifecycle::Active,
        PublicKey([5; 32]),
    );
    joiner_record.overlay_ip = "fd00::5".parse().map(OverlayIp).expect("valid overlay");
    joiner_record.storage_role = StorageParticipation::Candidate.into();
    joiner_record.endpoints = vec!["203.0.113.11:51820".into()];

    let ssh_dir = unique_temp_dir("ployz-fake-ssh");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let self_record_response = serde_json::to_string(&DaemonResponse::success(
        "self-record",
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            record: joiner_record.clone(),
        })),
    ))
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _status_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_STATUS_RESPONSE",
        Some(status_response_for_record(&joiner_record).into()),
    );
    let _ready_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_READY_RESPONSE",
        Some(
            "{\"status\":\"success\",\"code\":\"OK\",\"message\":\"ready\",\"payload\":{\"kind\":\"mesh-ready\",\"ready\":false,\"phase\":\"running\",\"store_healthy\":true,\"sync_connected\":false,\"workload_subnet_present\":true}}".into(),
        ),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.is_ok(), "{}", response.message());
    assert_eq!(response.code(), "MACHINE_ADD_FAILED");
    assert!(response.message().contains("failed_ready: 1"));
    assert!(
        response
            .message()
            .contains("timed out waiting for remote mesh readiness")
    );

    let machines = store.list_machines().await.expect("list machines");
    assert!(
        !machines
            .into_iter()
            .any(|machine| machine.id.as_str() == "joiner-2")
    );
    assert!(
        !network
            .current_peers()
            .into_iter()
            .any(|peer| peer.id().as_str() == "joiner-2")
    );

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_releases_reserved_subnet_when_operation_start_fails() {
    let (mut state, store, network) = make_state(true).await;
    let broken_root = unique_temp_dir("ployz-machine-operations-file");
    std::fs::write(&broken_root, b"not a directory").expect("write operations root file");
    state.data_dir = broken_root;

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.is_ok());
    assert_eq!(response.code(), "MACHINE_ADD_FAILED");
    assert!(response.message().contains("create machine operations dir"));

    let invites = store.list_invites().await.expect("list invites");
    assert_eq!(invites.len(), 1);
    assert!(
        invites
            .first()
            .expect("invite created")
            .status
            .consumed_by()
            .is_none()
    );
    assert!(network.current_peers().is_empty());

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn subnet_reservation_skips_currently_held_candidate() {
    let (mut state, _, _) = make_state(true).await;

    let first = state
        .reserve_machine_subnet(&MachineId::new("join-target"))
        .await
        .expect("first reservation");
    let first_subnet = first.subnet();
    let second = state
        .reserve_machine_subnet(&MachineId::new("join-target"))
        .await
        .expect("second reservation should skip held candidate");

    assert_ne!(first_subnet, second.subnet());

    release_reserved_subnet(second)
        .await
        .expect("release second");
    release_reserved_subnet(first).await.expect("release first");
    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_remove_refuses_offline_peer_without_force() {
    let (state, store, _) = make_state(false).await;
    store
        .upsert_self_machine(&test_machine_record(
            "peer-1",
            "10.210.1.0/24",
            MachineLifecycle::Active,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert peer");

    let response = state.handle_machine_remove("peer-1", false).await;
    assert!(!response.is_ok());
    assert_eq!(response.code(), "MACHINE_REMOVE_PEER_UNREACHABLE");
    assert!(response.message().contains("--force"));

    let machines = store.list_machines().await.expect("list machines");
    let peer = machines
        .into_iter()
        .find(|machine| machine.id.as_str() == "peer-1")
        .expect("failed unforced remove must preserve membership record");
    assert_eq!(peer.lifecycle, MachineLifecycle::Active);
    assert_eq!(
        peer.subnet,
        Some("10.210.1.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn machine_remove_rejects_invalid_target_id() {
    let (state, _, _) = make_state(false).await;

    let response = state.handle_machine_remove("bad machine", false).await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "MACHINE_INVALID_TARGET");
}

#[tokio::test]
async fn machine_remove_force_deletes_membership_record() {
    let (state, store, _) = make_state(false).await;
    store
        .upsert_self_machine(&test_machine_record(
            "peer-1",
            "10.210.1.0/24",
            MachineLifecycle::Standby,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert peer");

    let response = state.handle_machine_remove("peer-1", true).await;
    assert!(response.is_ok(), "{}", response.message());

    let machines = store.list_machines().await.expect("list machines");
    assert!(
        !machines
            .into_iter()
            .any(|machine| machine.id.as_str() == "peer-1")
    );
}

#[tokio::test]
async fn machine_update_rejects_invalid_target_id() {
    let (state, _, _) = make_state(false).await;

    let response = state
        .handle_machine_update(&["bad machine".into()], "latest", None)
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "MACHINE_UPDATE_INVALID_TARGET");
}

#[tokio::test]
async fn machine_drain_rejects_invalid_target_id() {
    let (mut state, _, _) = make_state(false).await;

    let response = state.handle_machine_drain("bad machine").await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "MACHINE_DRAIN_INVALID_TARGET");
}

#[tokio::test]
async fn machine_standby_rejects_invalid_target_id() {
    let (mut state, _, _) = make_state(false).await;

    let response = state.handle_machine_standby("bad machine", false).await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "MACHINE_STANDBY_INVALID_TARGET");
}

#[tokio::test]
async fn machine_drain_and_standby_route_to_exclusive_lane() {
    let (state, _, _) = make_state(true).await;
    assert_eq!(
        DaemonState::request_lane(&DaemonRequest::MachineDrain {
            target: "peer".into()
        }),
        RequestLane::Shared
    );
    assert_eq!(
        state.request_lane_for_state(&DaemonRequest::MachineDrain {
            target: "founder".into()
        }),
        RequestLane::Exclusive
    );
    assert_eq!(
        DaemonState::request_lane(&DaemonRequest::MachineStandby {
            target: "peer".into(),
            force: false,
        }),
        RequestLane::Shared
    );
    assert_eq!(
        state.request_lane_for_state(&DaemonRequest::MachineStandby {
            target: "founder".into(),
            force: false,
        }),
        RequestLane::Exclusive
    );
}

#[tokio::test]
async fn local_drain_preserves_subnet_and_marks_draining() {
    let (mut state, store, _) = make_state(true).await;
    mark_local_machine_active(&mut state).await;

    let response = state
        .transition_local_machine(MachineSelfTransition::Drain)
        .await;
    assert!(response.is_ok(), "{response:?}");

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.lifecycle, MachineLifecycle::Draining);
    assert_eq!(
        local.subnet,
        Some("10.210.0.0/24".parse().expect("valid subnet"))
    );
    assert_eq!(
        state.active.as_ref().expect("active mesh").config.subnet,
        Some("10.210.0.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn machine_drain_local_target_uses_local_transition() {
    let (mut state, store, _) = make_state(true).await;
    mark_local_machine_active(&mut state).await;

    let response = state.handle_machine_drain("founder").await;
    assert!(response.is_ok(), "{}", response.message());

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.lifecycle, MachineLifecycle::Draining);
}

async fn mark_local_machine_active(state: &mut DaemonState) {
    let active = state.active.as_mut().expect("active mesh");
    let Some(mut record) = active
        .mesh
        .update_authoritative_self_record(|record| {
            record.lifecycle = MachineLifecycle::Active;
            record.subnet = Some("10.210.0.0/24".parse().expect("valid subnet"));
        })
        .await
    else {
        panic!("self record missing");
    };
    record.lifecycle = MachineLifecycle::Active;
    record.subnet = Some("10.210.0.0/24".parse().expect("valid subnet"));
    active
        .mesh
        .store
        .upsert_self_machine(&record)
        .await
        .expect("persist active self");
}

#[tokio::test]
async fn local_standby_clears_subnet_and_marks_standby() {
    let (mut state, store, _) = make_state(true).await;
    let response = state
        .transition_local_machine(MachineSelfTransition::Standby { force: true })
        .await;
    assert!(response.is_ok(), "{response:?}");

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.lifecycle, MachineLifecycle::Standby);
    assert_eq!(local.subnet, None);
    assert_eq!(
        state.active.as_ref().expect("active mesh").config.subnet,
        None
    );
}

#[tokio::test]
async fn local_standby_requires_draining_without_force() {
    let (mut state, store, _) = make_state(true).await;
    {
        let active = state.active.as_mut().expect("active mesh");
        let Some(mut record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.lifecycle = MachineLifecycle::Active;
                record.subnet = Some("10.210.0.0/24".parse().expect("valid subnet"));
            })
            .await
        else {
            panic!("self record missing");
        };
        record.lifecycle = MachineLifecycle::Active;
        record.subnet = Some("10.210.0.0/24".parse().expect("valid subnet"));
        active
            .mesh
            .store
            .upsert_self_machine(&record)
            .await
            .expect("persist enabled self");
    }

    let response = state
        .transition_local_machine(MachineSelfTransition::Standby { force: false })
        .await;
    assert!(response.is_err());

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.lifecycle, MachineLifecycle::Active);
    assert_eq!(
        local.subnet,
        Some("10.210.0.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn machine_transition_self_surfaces_actionable_invalid_transition() {
    let (mut state, store, _) = make_state(true).await;
    {
        let active = state.active.as_mut().expect("active mesh");
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.lifecycle = MachineLifecycle::Active;
                record.subnet = Some("10.210.0.0/24".parse().expect("valid subnet"));
            })
            .await
        else {
            panic!("self record missing");
        };
        active
            .mesh
            .store
            .upsert_self_machine(&record)
            .await
            .expect("persist active self");
    }

    let response = state
        .handle_machine_transition_self(MachineSelfTransition::Standby { force: false })
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "INVALID_TRANSITION");
    assert!(
        response
            .message()
            .contains("machine must be draining before standby")
    );
    assert!(response.message().contains("--force"));

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.lifecycle, MachineLifecycle::Active);
    assert_eq!(
        local.subnet,
        Some("10.210.0.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn local_standby_restores_subnet_when_restart_fails() {
    let (mut state, _, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let original_config = NetworkConfig::load(&config_path).expect("load original config");
    state.gateway_listen_addr = "invalid".into();

    let response = state
        .transition_local_machine(MachineSelfTransition::Standby { force: true })
        .await;
    assert!(response.is_err());

    let restored_config = NetworkConfig::load(&config_path).expect("load restored config");
    assert_eq!(restored_config.subnet, original_config.subnet);
}

#[tokio::test]
async fn mesh_start_reactivates_local_machine_after_stop() {
    let (mut state, _, _) = make_state(true).await;

    let standby = state
        .transition_local_machine(MachineSelfTransition::Standby { force: true })
        .await;
    assert!(standby.is_ok(), "{standby:?}");

    let down = state.handle_mesh_stop(true).await;
    assert!(down.is_ok(), "{}", down.message());

    let up = state.handle_mesh_start("alpha").await;
    assert!(up.is_ok(), "{}", up.message());

    let local = state
        .active
        .as_ref()
        .expect("active mesh after up")
        .mesh
        .authoritative_self_record()
        .await
        .expect("self record");
    assert_eq!(local.lifecycle, MachineLifecycle::Active);
    assert_eq!(
        local.subnet,
        Some("10.210.0.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn mesh_stop_restores_local_record_when_destroy_fails() {
    let (mut state, store, network) = make_state(true).await;
    let activate = state
        .transition_local_machine(MachineSelfTransition::Activate {
            assigned_subnet: "10.210.0.0/24".parse().expect("valid subnet"),
        })
        .await;
    assert!(activate.is_ok(), "{activate:?}");

    network.set_fail_down(true);

    let response = state.handle_mesh_stop(true).await;
    assert!(!response.is_ok());
    assert_eq!(response.code(), "NETWORK_STOP_FAILED");
    assert!(state.active.is_some(), "active mesh should be restored");

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.lifecycle, MachineLifecycle::Active);
    assert_eq!(
        local.subnet,
        Some("10.210.0.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn mesh_stop_persists_stopped_lifecycle_before_gateway_shutdown_failure() {
    let (mut state, _, _) = make_state(true).await;
    let active = state.active.as_mut().expect("active mesh");
    active.gateway = Box::new(FailingShutdownHandle {
        message: "injected gateway shutdown failure".into(),
    });

    let response = state.handle_mesh_stop(true).await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "NETWORK_STOP_FAILED");
    assert!(
        response
            .message()
            .contains("injected gateway shutdown failure")
    );
    assert!(
        state.active.is_none(),
        "mesh runtime was already destroyed, so active state must not be restored"
    );

    let persisted =
        NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha")).expect("load config");
    assert_eq!(persisted.lifecycle, NetworkLifecycle::Stopped);
    assert_eq!(
        persisted.subnet,
        Some("10.210.0.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn machine_add_releases_reserved_subnet_when_remote_bootstrap_fails() {
    let _guard = test_ssh_env_lock().lock().await;
    let (state, _, _) = make_state(true).await;

    let ssh_dir = unique_temp_dir("ployz-fake-ssh-bootstrap-fail");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let _status_fail_guard =
        TestSshEnvGuard::set("PLOYZ_TEST_STATUS_FAIL_TARGETS", Some("join-target".into()));

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.is_ok(), "{}", response.message());
    assert_eq!(response.code(), "MACHINE_ADD_FAILED");
}

#[tokio::test]
async fn machine_add_rejects_remote_subnet_mismatch_before_invite_consume() {
    let _guard = test_ssh_env_lock().lock().await;
    let (state, store, _) = make_state(true).await;

    let mut joiner_record = test_machine_record(
        "joiner-mismatch",
        "10.210.99.0/24",
        MachineLifecycle::Active,
        PublicKey([14; 32]),
    );
    joiner_record.overlay_ip = "fd00::14".parse().map(OverlayIp).expect("valid overlay");
    joiner_record.storage_role = StorageParticipation::Candidate.into();
    joiner_record.endpoints = vec!["203.0.113.14:51820".into()];

    let ssh_dir = unique_temp_dir("ployz-fake-ssh-mismatch");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let self_record_response = serde_json::to_string(&DaemonResponse::success(
        "self-record",
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            record: joiner_record.clone(),
        })),
    ))
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _status_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_STATUS_RESPONSE",
        Some(status_response_for_record(&joiner_record).into()),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.is_ok(), "{}", response.message());
    assert_eq!(response.code(), "MACHINE_ADD_FAILED");
    assert!(response.message().contains("founder reserved"));

    let invites = store.list_invites().await.expect("list invites");
    assert_eq!(invites.len(), 1);
    assert_eq!(
        invites
            .first()
            .expect("invite created")
            .status
            .consumed_by(),
        None
    );
}

#[tokio::test]
async fn machine_add_rejects_remote_authority_posture_before_invite_consume() {
    let _guard = test_ssh_env_lock().lock().await;
    let (state, store, _) = make_state(true).await;

    let reserved = state
        .reserve_machine_subnet(&MachineId::new("join-target"))
        .await
        .expect("reserve expected subnet");
    let expected_subnet = reserved.subnet();
    release_reserved_subnet(reserved)
        .await
        .expect("release reserved subnet for re-acquire");

    let mut joiner_record = test_machine_record(
        "joiner-authority",
        &expected_subnet.to_string(),
        MachineLifecycle::Active,
        PublicKey([15; 32]),
    );
    joiner_record.overlay_ip = "fd00::15".parse().map(OverlayIp).expect("valid overlay");
    joiner_record.storage_role = StorageParticipation::default_authority().into();
    joiner_record.endpoints = vec!["203.0.113.15:51820".into()];

    let ssh_dir = unique_temp_dir("ployz-fake-ssh-authority-posture");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let self_record_response = serde_json::to_string(&DaemonResponse::success(
        "self-record",
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            record: joiner_record.clone(),
        })),
    ))
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _status_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_STATUS_RESPONSE",
        Some(status_response_for_record(&joiner_record).into()),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.is_ok(), "{}", response.message());
    assert_eq!(response.code(), "MACHINE_ADD_FAILED");
    assert!(response.message().contains("reported authority storage"));

    let invites = store.list_invites().await.expect("list invites");
    assert_eq!(invites.len(), 1);
    assert_eq!(
        invites
            .first()
            .expect("invite created")
            .status
            .consumed_by(),
        None
    );
    let machines = store.list_machines().await.expect("list machines");
    assert!(
        !machines
            .into_iter()
            .any(|machine| machine.id.as_str() == "joiner-authority")
    );
}

#[tokio::test]
async fn machine_storage_promote_marks_active_candidates_as_authority_storage() {
    let (mut state, store, _) = make_state(true).await;
    let founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    store
        .upsert_self_machine(&founder)
        .await
        .expect("upsert active founder");
    let mut candidate_a = test_machine_record(
        "candidate-a",
        "10.210.2.0/24",
        MachineLifecycle::Active,
        PublicKey([16; 32]),
    );
    candidate_a.storage_role = StorageParticipation::Candidate.into();
    let mut candidate_b = test_machine_record(
        "candidate-b",
        "10.210.3.0/24",
        MachineLifecycle::Active,
        PublicKey([17; 32]),
    );
    candidate_b.storage_role = StorageParticipation::Candidate.into();
    store
        .upsert_self_machine(&candidate_a)
        .await
        .expect("upsert candidate a");
    store
        .upsert_self_machine(&candidate_b)
        .await
        .expect("upsert candidate b");

    let response = state
        .handle_machine_storage_promote(
            &MachineStoragePromoteRequest {
                targets: vec!["candidate-a".into(), "candidate-b".into()],
                replicas: StorageReplicaPolicy::R3,
            },
            None,
        )
        .await;

    assert!(response.is_ok(), "{}", response.message());
    let Some(DaemonPayload::MachineStoragePromotion(payload)) = response.payload() else {
        panic!("expected storage promotion payload");
    };
    assert_eq!(payload.replicas, StorageReplicaPolicy::R3);
    assert_eq!(payload.promoted, vec!["candidate-a", "candidate-b"]);

    let machines = store.list_machines().await.expect("list machines");
    let authority_count = machines
        .iter()
        .filter(|machine| machine.storage_participation().is_authority())
        .count();
    assert_eq!(authority_count, 3);
    let config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("load network config");
    assert_eq!(config.storage_replicas, StorageReplicaPolicy::R3);
    let peers = load_bootstrap_peer_records(&state.network_dir("alpha"))
        .expect("load bootstrap peers after promotion");
    let peer_ids = peers
        .iter()
        .map(|peer| peer.machine_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(peer_ids, vec!["candidate-a", "candidate-b", "founder"]);
}

#[tokio::test]
async fn machine_storage_promote_rejects_wrong_final_authority_count() {
    let (mut state, store, _) = make_state(true).await;
    let founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    store
        .upsert_self_machine(&founder)
        .await
        .expect("upsert active founder");
    let mut candidate = test_machine_record(
        "candidate-a",
        "10.210.2.0/24",
        MachineLifecycle::Active,
        PublicKey([16; 32]),
    );
    candidate.storage_role = StorageParticipation::Candidate.into();
    store
        .upsert_self_machine(&candidate)
        .await
        .expect("upsert candidate");

    let response = state
        .handle_machine_storage_promote(
            &MachineStoragePromoteRequest {
                targets: vec!["candidate-a".into()],
                replicas: StorageReplicaPolicy::R3,
            },
            None,
        )
        .await;

    assert!(!response.is_ok(), "{}", response.message());
    assert_eq!(response.code(), "MACHINE_STORAGE_PROMOTION_FAILED");
    assert!(response.message().contains("requires exactly 3 active"));
    let machines = store.list_machines().await.expect("list machines");
    let promoted = machines
        .iter()
        .find(|machine| machine.id.as_str() == "candidate-a")
        .expect("candidate remains present");
    assert_eq!(
        promoted.storage_participation(),
        StorageParticipation::Candidate
    );
}

#[tokio::test]
async fn machine_storage_promote_rejects_duplicate_targets_without_changing_intent() {
    let (mut state, store, _) = make_state(true).await;
    let founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    store
        .upsert_self_machine(&founder)
        .await
        .expect("upsert active founder");
    let mut candidate = test_machine_record(
        "candidate-a",
        "10.210.2.0/24",
        MachineLifecycle::Active,
        PublicKey([16; 32]),
    );
    candidate.storage_role = StorageParticipation::Candidate.into();
    store
        .upsert_self_machine(&candidate)
        .await
        .expect("upsert candidate");

    let response = state
        .handle_machine_storage_promote(
            &MachineStoragePromoteRequest {
                targets: vec!["candidate-a".into(), "candidate-a".into()],
                replicas: StorageReplicaPolicy::R3,
            },
            None,
        )
        .await;

    assert!(!response.is_ok(), "{}", response.message());
    assert_eq!(response.code(), "MACHINE_STORAGE_PROMOTION_FAILED");
    let Some(DaemonPayload::MachineStoragePromotion(payload)) = response.payload() else {
        panic!("expected storage promotion payload");
    };
    assert_eq!(payload.promoted, Vec::<String>::new());
    assert_eq!(payload.failed.len(), 1);
    assert_eq!(payload.failed[0].machine_id, "candidate-a");
    assert!(payload.failed[0].message.contains("duplicate"));

    let machines = store.list_machines().await.expect("list machines");
    let candidate = machines
        .iter()
        .find(|machine| machine.id.as_str() == "candidate-a")
        .expect("candidate remains present");
    assert_eq!(
        candidate.storage_participation(),
        StorageParticipation::Candidate
    );
    let config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("load network config");
    assert_eq!(config.storage_replicas, StorageReplicaPolicy::Single);
}

#[tokio::test]
async fn machine_storage_promote_rejects_invalid_targets_without_changing_intent() {
    let (mut state, store, _) = make_state(true).await;
    let founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    store
        .upsert_self_machine(&founder)
        .await
        .expect("upsert active founder");
    let mut inactive = test_machine_record(
        "inactive-candidate",
        "10.210.2.0/24",
        MachineLifecycle::Standby,
        PublicKey([16; 32]),
    );
    inactive.storage_role = StorageParticipation::Candidate.into();
    let mut compute = test_machine_record(
        "compute-candidate",
        "10.210.3.0/24",
        MachineLifecycle::Active,
        PublicKey([17; 32]),
    );
    compute.storage_role = MachineStorageRole::Compute;
    let existing_authority = test_machine_record(
        "existing-authority",
        "10.210.4.0/24",
        MachineLifecycle::Active,
        PublicKey([18; 32]),
    );
    for machine in [&inactive, &compute, &existing_authority] {
        store
            .upsert_self_machine(machine)
            .await
            .expect("upsert invalid target");
    }

    let response = state
        .handle_machine_storage_promote(
            &MachineStoragePromoteRequest {
                targets: vec![
                    "inactive-candidate".into(),
                    "compute-candidate".into(),
                    "existing-authority".into(),
                    "missing-candidate".into(),
                ],
                replicas: StorageReplicaPolicy::R5,
            },
            None,
        )
        .await;

    assert!(!response.is_ok(), "{}", response.message());
    assert_eq!(response.code(), "MACHINE_STORAGE_PROMOTION_FAILED");
    let Some(DaemonPayload::MachineStoragePromotion(payload)) = response.payload() else {
        panic!("expected storage promotion payload");
    };
    assert_eq!(payload.promoted, Vec::<String>::new());
    assert_eq!(payload.failed.len(), 4);
    assert!(
        payload
            .failed
            .iter()
            .any(|failure| failure.machine_id == "missing-candidate")
    );

    let machines = store.list_machines().await.expect("list machines");
    let inactive = machines
        .iter()
        .find(|machine| machine.id.as_str() == "inactive-candidate")
        .expect("inactive candidate remains present");
    assert_eq!(inactive.lifecycle, MachineLifecycle::Standby);
    assert_eq!(
        inactive.storage_participation(),
        StorageParticipation::Candidate
    );
    let compute = machines
        .iter()
        .find(|machine| machine.id.as_str() == "compute-candidate")
        .expect("compute candidate remains present");
    assert!(!compute.storage());
    assert_eq!(
        compute.storage_participation(),
        StorageParticipation::Candidate
    );
    let config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("load network config");
    assert_eq!(config.storage_replicas, StorageReplicaPolicy::Single);
}

#[tokio::test]
async fn machine_storage_promote_self_persists_config_bootstrap_peers_and_self_record() {
    let (mut state, store, _) = make_state(true).await;
    let mut config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("load network config");
    config.storage = false;
    config.storage_participation = StorageParticipation::Candidate;
    config
        .save(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("save candidate config");
    state.active.as_mut().expect("active mesh").config = config;

    let mut founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    founder.storage_role = StorageParticipation::Candidate.into();
    store
        .upsert_self_machine(&founder)
        .await
        .expect("upsert candidate founder");
    let mut authority_founder = founder.clone();
    authority_founder.storage_role = StorageParticipation::default_authority().into();

    let peer_a = test_machine_record(
        "peer-a",
        "10.210.2.0/24",
        MachineLifecycle::Active,
        PublicKey([16; 32]),
    );
    let peer_b = test_machine_record(
        "peer-b",
        "10.210.3.0/24",
        MachineLifecycle::Active,
        PublicKey([17; 32]),
    );
    store
        .upsert_self_machine(&peer_a)
        .await
        .expect("upsert peer a");
    store
        .upsert_self_machine(&peer_b)
        .await
        .expect("upsert peer b");
    let authority_peers = [&authority_founder, &peer_a, &peer_b]
        .into_iter()
        .map(MachineStorageAuthorityPeer::from)
        .collect::<Vec<_>>();

    let response = state
        .handle_machine_storage_promote_self(StorageReplicaPolicy::R3, &authority_peers)
        .await;

    assert!(response.is_ok(), "{}", response.message());
    let config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("load promoted config");
    assert!(config.storage);
    assert_eq!(
        config.storage_participation,
        StorageParticipation::default_authority()
    );
    assert_eq!(config.storage_replicas, StorageReplicaPolicy::R3);
    let peers = load_bootstrap_peer_records(&state.network_dir("alpha"))
        .expect("load bootstrap peers after self promotion");
    let peer_ids = peers
        .iter()
        .map(|peer| peer.machine_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(peer_ids, vec!["founder", "peer-a", "peer-b"]);
    let self_record = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .authoritative_self_record()
        .await
        .expect("self record");
    assert!(self_record.storage());
    assert_eq!(
        self_record.storage_participation(),
        StorageParticipation::default_authority()
    );
}

#[tokio::test]
async fn machine_storage_promote_self_rejects_malformed_bootstrap_peers_before_config_mutation() {
    let (mut state, _, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let mut config = NetworkConfig::load(&config_path).expect("load network config");
    config.storage = false;
    config.storage_participation = StorageParticipation::Candidate;
    config.storage_replicas = StorageReplicaPolicy::Single;
    config.save(&config_path).expect("save candidate config");
    state.active.as_mut().expect("active mesh").config = config.clone();
    std::fs::write(
        bootstrap_peers_path(&state.network_dir("alpha")),
        "not json",
    )
    .expect("write malformed peers");

    let authority_peers = [test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    )]
    .iter()
    .map(MachineStorageAuthorityPeer::from)
    .collect::<Vec<_>>();

    let response = state
        .handle_machine_storage_promote_self(StorageReplicaPolicy::R3, &authority_peers)
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "IO_ERROR");
    let restored = NetworkConfig::load(&config_path).expect("load config after failure");
    assert!(!restored.storage);
    assert_eq!(
        restored.storage_participation,
        StorageParticipation::Candidate
    );
    assert_eq!(restored.storage_replicas, StorageReplicaPolicy::Single);
}

#[tokio::test]
async fn machine_storage_promote_self_rejects_invalid_authority_peer_payload_before_config_mutation()
 {
    let (mut state, _, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let mut config = NetworkConfig::load(&config_path).expect("load network config");
    config.storage = false;
    config.storage_participation = StorageParticipation::Candidate;
    config.storage_replicas = StorageReplicaPolicy::Single;
    config.save(&config_path).expect("save candidate config");
    state.active.as_mut().expect("active mesh").config = config.clone();

    let authority_peers = [test_machine_record(
        "some-other-peer",
        "10.210.2.0/24",
        MachineLifecycle::Active,
        PublicKey([16; 32]),
    )]
    .iter()
    .map(MachineStorageAuthorityPeer::from)
    .collect::<Vec<_>>();

    let response = state
        .handle_machine_storage_promote_self(StorageReplicaPolicy::R3, &authority_peers)
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "INVALID_AUTHORITY_PEERS");
    let restored = NetworkConfig::load(&config_path).expect("load config after failure");
    assert!(!restored.storage);
    assert_eq!(
        restored.storage_participation,
        StorageParticipation::Candidate
    );
    assert_eq!(restored.storage_replicas, StorageReplicaPolicy::Single);
}

#[tokio::test]
async fn machine_storage_promote_self_rejects_authority_peers_that_do_not_match_membership() {
    let (mut state, store, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let mut config = NetworkConfig::load(&config_path).expect("load network config");
    config.storage = true;
    config.storage_participation = StorageParticipation::Candidate;
    config.storage_replicas = StorageReplicaPolicy::Single;
    config.save(&config_path).expect("save candidate config");
    state.active.as_mut().expect("active mesh").config = config;

    let mut founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    founder.storage_role = StorageParticipation::Candidate.into();
    store
        .upsert_self_machine(&founder)
        .await
        .expect("upsert founder");
    let peer_a = test_machine_record(
        "peer-a",
        "10.210.2.0/24",
        MachineLifecycle::Active,
        PublicKey([16; 32]),
    );
    store
        .upsert_self_machine(&peer_a)
        .await
        .expect("upsert peer a");
    let peer_b = test_machine_record(
        "peer-b",
        "10.210.3.0/24",
        MachineLifecycle::Active,
        PublicKey([17; 32]),
    );
    store
        .upsert_self_machine(&peer_b)
        .await
        .expect("upsert peer b");

    let mut authority_peers = [&founder, &peer_a, &peer_b]
        .into_iter()
        .map(MachineStorageAuthorityPeer::from)
        .collect::<Vec<_>>();
    authority_peers[0].public_key = PublicKey([99; 32]);

    let response = state
        .handle_machine_storage_promote_self(StorageReplicaPolicy::R3, &authority_peers)
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "INVALID_AUTHORITY_PEERS");
    let restored = NetworkConfig::load(&config_path).expect("load config after failure");
    assert_eq!(
        restored.storage_participation,
        StorageParticipation::Candidate
    );
    assert_eq!(restored.storage_replicas, StorageReplicaPolicy::Single);
}

#[tokio::test]
async fn machine_storage_promote_self_accepts_unsynced_nonlocal_authority_peers() {
    let (mut state, store, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let mut config = NetworkConfig::load(&config_path).expect("load network config");
    config.storage = false;
    config.storage_participation = StorageParticipation::Candidate;
    config.storage_replicas = StorageReplicaPolicy::Single;
    config.save(&config_path).expect("save candidate config");
    state.active.as_mut().expect("active mesh").config = config;

    let mut founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    founder.storage_role = StorageParticipation::Candidate.into();
    store
        .upsert_self_machine(&founder)
        .await
        .expect("upsert founder");
    let mut authority_founder = founder.clone();
    authority_founder.storage_role = StorageParticipation::default_authority().into();
    let peer_a = test_machine_record(
        "peer-a",
        "10.210.2.0/24",
        MachineLifecycle::Active,
        PublicKey([16; 32]),
    );
    let peer_b = test_machine_record(
        "peer-b",
        "10.210.3.0/24",
        MachineLifecycle::Active,
        PublicKey([17; 32]),
    );
    let authority_peers = [&authority_founder, &peer_a, &peer_b]
        .into_iter()
        .map(MachineStorageAuthorityPeer::from)
        .collect::<Vec<_>>();

    let response = state
        .handle_machine_storage_promote_self(StorageReplicaPolicy::R3, &authority_peers)
        .await;

    assert!(response.is_ok(), "{}", response.message());
    let peers = load_bootstrap_peer_records(&state.network_dir("alpha"))
        .expect("load bootstrap peers after self promotion");
    let peer_ids = peers
        .iter()
        .map(|peer| peer.machine_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(peer_ids, vec!["founder", "peer-a", "peer-b"]);
}

#[tokio::test]
async fn machine_storage_restore_self_restores_config_bootstrap_peers_and_self_record() {
    let (mut state, _, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let mut config = NetworkConfig::load(&config_path).expect("load network config");
    config.storage = true;
    config.storage_participation = StorageParticipation::default_authority();
    config.storage_replicas = StorageReplicaPolicy::R3;
    config.save(&config_path).expect("save promoted config");
    state.active.as_mut().expect("active mesh").config = config;

    let mut founder = test_machine_record(
        "founder",
        "10.210.0.0/24",
        MachineLifecycle::Active,
        PublicKey([1; 32]),
    );
    founder.storage_role = StorageParticipation::default_authority().into();

    let response = state
        .handle_machine_storage_restore_self(
            StorageParticipation::Candidate,
            StorageReplicaPolicy::Single,
            &[founder],
        )
        .await;

    assert!(response.is_ok(), "{}", response.message());
    let restored_config = NetworkConfig::load(&config_path).expect("load restored config");
    assert_eq!(
        restored_config.storage_participation,
        StorageParticipation::Candidate
    );
    assert_eq!(
        restored_config.storage_replicas,
        StorageReplicaPolicy::Single
    );
    let peers = load_bootstrap_peer_records(&state.network_dir("alpha"))
        .expect("load restored bootstrap peers");
    let peer_ids = peers
        .iter()
        .map(|peer| peer.machine_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(peer_ids, vec!["founder"]);
    let self_record = state
        .active
        .as_ref()
        .expect("active mesh")
        .mesh
        .authoritative_self_record()
        .await
        .expect("self record");
    assert_eq!(
        self_record.storage_participation(),
        StorageParticipation::Candidate
    );
}

#[tokio::test]
async fn local_activate_restores_subnet_before_active_finalization() {
    let (mut state, store, _) = make_state(true).await;
    let standby_response = state
        .transition_local_machine(MachineSelfTransition::Standby { force: true })
        .await;
    assert!(standby_response.is_ok(), "{standby_response:?}");

    let activate_response = state
        .transition_local_machine(MachineSelfTransition::Activate {
            assigned_subnet: "10.210.2.0/24".parse().expect("valid subnet"),
        })
        .await;
    assert!(activate_response.is_ok(), "{activate_response:?}");

    let promoted = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(promoted.lifecycle, MachineLifecycle::Active);
    assert_eq!(
        promoted.subnet,
        Some("10.210.2.0/24".parse().expect("valid subnet"))
    );
    assert_eq!(
        state.active.as_ref().expect("active mesh").config.subnet,
        Some("10.210.2.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn interrupted_machine_add_is_marked_interrupted_on_startup() {
    let _guard = test_ssh_env_lock().lock().await;
    let (state, _, _) = make_state(false).await;
    let ssh_dir = unique_temp_dir("ployz-fake-ssh-recovery");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
    let store = state.machine_operation_store();
    let mut operation = store
        .begin(
            MachineOperationKind::Add,
            Some("alpha".into()),
            vec!["join-target".into()],
            "bootstrap-published",
            MachineOperationArtifacts {
                machine_id: Some(MachineId::new("joiner-1")),
                invite_id: Some("invite-1".into()),
                allocated_subnet: Some("10.210.2.0/24".into()),
                ..MachineOperationArtifacts::default()
            },
        )
        .expect("begin operation");
    store
        .update_status(&mut operation, MachineOperationStatus::Running, None)
        .expect("keep running");

    state.recover_machine_operations_on_startup().await;

    let recovered = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(recovered.status(), MachineOperationStatus::Interrupted);
    assert!(
        recovered
            .last_error()
            .expect("last error")
            .contains("daemon restarted")
    );
    assert!(
        recovered
            .last_error()
            .expect("last error")
            .contains("remote cleanup attempted for 'join-target'")
    );
}

#[tokio::test]
async fn interrupted_joined_machine_add_keeps_remote_cleanup_eligible_on_startup() {
    let (state, _, _) = make_state(false).await;
    let store = state.machine_operation_store();
    let mut operation = store
        .begin(
            MachineOperationKind::Add,
            Some("alpha".into()),
            vec!["join-target".into()],
            "joined",
            MachineOperationArtifacts {
                machine_id: Some(MachineId::new("joiner-1")),
                invite_id: Some("invite-1".into()),
                allocated_subnet: Some("10.210.2.0/24".into()),
                uses_operation_identity: true,
                ..MachineOperationArtifacts::default()
            },
        )
        .expect("begin operation");
    store
        .update_status(&mut operation, MachineOperationStatus::Running, None)
        .expect("keep running");

    state.recover_machine_operations_on_startup().await;

    let recovered = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(recovered.status(), MachineOperationStatus::Interrupted);
    assert!(
        recovered
            .last_error()
            .expect("last error")
            .contains("operation-scoped ssh identity is unavailable")
    );
}

#[tokio::test]
async fn interrupted_machine_add_attempts_remote_cleanup_without_active_mesh_on_startup() {
    let _guard = test_ssh_env_lock().lock().await;
    let (mut state, _, _) = make_state(false).await;
    state.active = None;
    let ssh_dir = unique_temp_dir("ployz-fake-ssh-recovery-no-active");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
    let store = state.machine_operation_store();
    let mut operation = store
        .begin(
            MachineOperationKind::Add,
            Some("alpha".into()),
            vec!["join-target".into()],
            "bootstrap-published",
            MachineOperationArtifacts {
                machine_id: Some(MachineId::new("joiner-1")),
                invite_id: Some("invite-1".into()),
                allocated_subnet: Some("10.210.2.0/24".into()),
                ..MachineOperationArtifacts::default()
            },
        )
        .expect("begin operation");
    store
        .update_status(&mut operation, MachineOperationStatus::Running, None)
        .expect("keep running");

    state.recover_machine_operations_on_startup().await;

    let recovered = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(recovered.status(), MachineOperationStatus::Interrupted);
    let last_error = recovered.last_error().expect("last error");
    assert!(last_error.contains("bootstrap membership cleanup skipped: no running network"));
    assert!(last_error.contains("remote cleanup attempted for 'join-target'"));
}

#[tokio::test]
async fn interrupted_latest_machine_update_without_version_change_stays_interrupted_on_startup() {
    let (state, _, _) = make_state(false).await;
    let store = state.machine_operation_store();
    let operation = store
        .begin(
            MachineOperationKind::Update,
            Some("alpha".into()),
            vec!["founder".into()],
            "installing",
            MachineOperationArtifacts {
                requested_version: Some("latest".into()),
                previous_version: Some(env!("CARGO_PKG_VERSION").into()),
                ..MachineOperationArtifacts::default()
            },
        )
        .expect("begin operation");

    state.recover_machine_operations_on_startup().await;

    let recovered = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(recovered.status(), MachineOperationStatus::Interrupted);
    assert!(
        recovered
            .last_error()
            .expect("last error")
            .contains("expected latest")
    );
}

#[tokio::test]
async fn interrupted_latest_machine_update_with_version_change_succeeds_on_startup() {
    let (state, _, _) = make_state(false).await;
    let store = state.machine_operation_store();
    let operation = store
        .begin(
            MachineOperationKind::Update,
            Some("alpha".into()),
            vec!["founder".into()],
            "installing",
            MachineOperationArtifacts {
                requested_version: Some("latest".into()),
                previous_version: Some("0.0.0-before-update".into()),
                ..MachineOperationArtifacts::default()
            },
        )
        .expect("begin operation");

    state.recover_machine_operations_on_startup().await;

    let recovered = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(recovered.status(), MachineOperationStatus::Succeeded);
    assert!(recovered.last_error().is_none());
}

#[tokio::test]
async fn interrupted_pinned_machine_update_requires_matching_version_on_startup() {
    let (state, _, _) = make_state(false).await;
    let store = state.machine_operation_store();
    let operation = store
        .begin(
            MachineOperationKind::Update,
            Some("alpha".into()),
            vec!["founder".into()],
            "installing",
            MachineOperationArtifacts {
                requested_version: Some("999.999.999".into()),
                ..MachineOperationArtifacts::default()
            },
        )
        .expect("begin operation");

    state.recover_machine_operations_on_startup().await;

    let recovered = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(recovered.status(), MachineOperationStatus::Interrupted);
    assert!(
        recovered
            .last_error()
            .expect("last error")
            .contains("expected 999.999.999")
    );
}

async fn make_state(start_mesh: bool) -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
    make_state_with_zfs_transfer_port(start_mesh, 4319).await
}

async fn make_state_with_zfs_transfer_port(
    start_mesh: bool,
    zfs_transfer_port: u16,
) -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
    let identity = Identity::generate(MachineId::new("founder"), [1; 32]);
    let founder_subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
    let data_dir = unique_temp_dir("ployz-machine-state");
    let config = NetworkConfig::new(
        ployz_types::model::NetworkName("alpha".into()),
        &identity.public_key,
        DEFAULT_CLUSTER_CIDR,
        founder_subnet,
    );
    config
        .save(&NetworkConfig::path(&data_dir, "alpha"))
        .expect("save config");

    let store = Arc::new(MemoryStore::new());
    let service = Arc::new(MemoryService::new());
    let network = Arc::new(MemoryWireGuard::new());
    let founder_record = test_machine_record(
        "founder",
        "10.210.0.0/24",
        if start_mesh {
            MachineLifecycle::Active
        } else {
            MachineLifecycle::Standby
        },
        identity.public_key.clone(),
    );
    store
        .upsert_self_machine(&founder_record)
        .await
        .expect("upsert founder");

    let mut mesh = Mesh::new(
        WireguardDriver::memory_with(network.clone()),
        StoreDriver::memory_with(store.clone(), service),
        None,
        identity.machine_id.clone(),
        51820,
    );
    if start_mesh {
        mesh.up().await.expect("mesh up");
    }

    let mut state = DaemonState::new_for_tests(
        &data_dir,
        identity,
        DEFAULT_CLUSTER_CIDR.into(),
        24,
        zfs_transfer_port,
        "127.0.0.1:0".into(),
        None,
        1,
    );
    let mut config = config;
    config.lifecycle = if start_mesh {
        NetworkLifecycle::Running
    } else {
        NetworkLifecycle::Stopped
    };
    let retained_subnet = crate::daemon::RetainedSubnet::from_running_config(config.subnet);
    state.active = Some(ActiveMesh {
        config,
        retained_subnet,
        mesh,
        nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        image_receiver: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        image_receiver_bind_addr: None,
        gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        certificate_renewal: None,
        bootstrap_peer_seed: None,
    });

    (state, store, network)
}

async fn teardown_state(state: &mut DaemonState) {
    let Some(active) = state.active.as_mut() else {
        return;
    };
    active.mesh.destroy().await.expect("destroy mesh");
}

struct FailingShutdownHandle {
    message: String,
}

#[async_trait::async_trait]
impl ployz_runtime_api::RuntimeHandle for FailingShutdownHandle {
    async fn shutdown(self: Box<Self>) -> Result<(), String> {
        Err(self.message)
    }
}

fn test_machine_record(
    id: &str,
    subnet: &str,
    lifecycle: MachineLifecycle,
    public_key: PublicKey,
) -> MachineMembership {
    MachineMembership {
        id: MachineId::new(id),
        public_key,
        overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
            .parse()
            .map(OverlayIp)
            .expect("valid overlay"),
        topology: MachineTopology::local(),
        region_role: ployz_types::model::RegionRole::HomeData,
        subnet: match lifecycle {
            MachineLifecycle::Active | MachineLifecycle::Draining => {
                Some(subnet.parse().expect("valid subnet"))
            }
            MachineLifecycle::Standby => None,
        },
        bridge_ip: None,
        endpoints: vec!["127.0.0.1:51820".into()],
        lifecycle,
        storage_role: StorageParticipation::default_authority().into(),
        created_at: 0,
        updated_at: 0,
        labels: std::collections::BTreeMap::new(),
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
}

fn status_response_for_record(record: &MachineMembership) -> String {
    serde_json::to_string(&DaemonResponse::success(
        "status",
        Some(DaemonPayload::Status(StatusPayload {
            machine_id: record.id.as_str().to_string(),
            public_key: record.public_key.clone(),
            version: "test-version".into(),
            network: None,
            overlay_ip: None,
            network_lifecycle: None,
            local_machine_lifecycle: None,
            local_authority: None,
            mesh_phase: "idle".into(),
            edge_sync: Vec::new(),
            nats_assets: Vec::new(),
            control_plane: Vec::new(),
        })),
    ))
    .expect("encode status response")
}

fn write_fake_ssh(dir: &Path) -> PathBuf {
    let script = dir.join("ssh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprev=''\nfor arg in \"$@\"; do\n  target=\"$prev\"\n  command=\"$arg\"\n  prev=\"$arg\"\ndone\nif [ \"$command\" = 'set -eu; \"$HOME/.local/bin/ployzctl\" rpc-stdio' ]; then\n  req=$(cat)\n  case \"$req\" in\n    *'\"Status\"'*)\n      printf '%s' \"$PLOYZ_TEST_STATUS_RESPONSE\"\n      ;;\n    *'\"MeshBootstrap\"'*)\n      printf '{\"status\":\"success\",\"code\":\"OK\",\"message\":\"bootstrapped\"}'\n      ;;\n    *'\"MeshJoin\"'*)\n      printf '{\"status\":\"error\",\"code\":\"UNSUPPORTED\",\"message\":\"unsupported\"}'\n      ;;\n    *'\"MeshInit\"'*)\n      printf '{\"status\":\"success\",\"code\":\"OK\",\"message\":\"init\"}'\n      ;;\n    *'\"MeshDestroy\"'*)\n      printf '{\"status\":\"success\",\"code\":\"OK\",\"message\":\"destroyed\"}'\n      ;;\n    *'\"MeshDown\"'*)\n      printf '{\"status\":\"success\",\"code\":\"OK\",\"message\":\"down\"}'\n      ;;\n    *'\"MeshSelfRecord\"'*)\n      printf '%s' \"$PLOYZ_TEST_SELF_RECORD_RESPONSE\"\n      ;;\n    *'\"MeshReady\"'*)\n      printf '%s' \"$PLOYZ_TEST_READY_RESPONSE\"\n      ;;\n    *)\n      printf '{\"status\":\"success\",\"code\":\"OK\",\"message\":\"ok\"}'\n      ;;\n  esac\n  exit 0\nfi\ncase \"$command\" in\n  *'--version'*)\n    printf 'ployzctl test-version'\n    exit 0\n    ;;\n  *'status >/dev/null'*)\n    case \",$PLOYZ_TEST_STATUS_FAIL_TARGETS,\" in\n      *\",$target,\"*) exit 1 ;;\n      *) exit 0 ;;\n    esac\n    ;;\n  *'bash -s -- install'*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
    )
    .expect("write fake ssh");

    #[cfg(unix)]
    {
        let mut permissions = std::fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("set script permissions");
    }

    script
}
