use super::join::release_reserved_subnet;
use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use crate::daemon::ActiveMesh;
use crate::daemon::DaemonState;
use crate::daemon::ssh::{TestSshEnvGuard, TestSshProgramGuard, test_ssh_env_lock};
use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
use ipnet::Ipv4Net;
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineAddOptions, MachineTransitionGoal,
    MeshReadyPayload, MeshSelfRecordPayload, StatusPayload,
};
use ployz_orchestrator::Mesh;
use ployz_orchestrator::mesh::driver::WireguardDriver;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_runtime_api::Identity;
use ployz_store_api::StoreDriver;
use ployz_store_api::memory::{MemoryService, MemoryStore};
use ployz_store_api::{InviteRepository, MachineRegistry};
use ployz_types::model::{
    JoinResponse, MachineId, MachineLifecycle, MachineMembership, MachineRole, MachineTopology,
    NetworkLifecycle, OverlayIp, PublicKey,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

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
    assert!(response.ok);
    assert!(!response.message.contains("LIVENESS"));
    assert!(response.message.contains("peer-disabled"));
    assert!(response.message.contains("disabled"));
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
    assert!(response.ok);
    assert!(response.message.contains("peer-active"));
    assert!(response.message.contains("active"));
}

#[tokio::test]
async fn machine_list_json_payload_contains_rows() {
    let (state, _, _) = make_state(false).await;
    let response = state.handle_machine_list().await;
    let Some(DaemonPayload::MachineList(payload)) = response.payload else {
        panic!("expected machine list payload");
    };
    assert_eq!(payload.rows.len(), 1);
    assert_eq!(payload.rows.first().expect("founder row").id, "founder");
}

#[tokio::test]
async fn machine_rtt_does_not_fan_out_to_unreachable_peer() {
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("bind overlay listener");
    let peer_rpc_port = listener.local_addr().expect("listener addr").port();
    drop(listener);
    let remote_control_port = peer_rpc_port
        .checked_sub(1)
        .expect("peer rpc port has preceding remote control port");
    let (mut state, store, _) = make_state_with_remote_port(true, remote_control_port).await;

    let mut peer = test_machine_record(
        "peer-1",
        "10.210.1.0/24",
        MachineLifecycle::Active,
        PublicKey([2; 32]),
    );
    peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    store.upsert_self_machine(&peer).await.expect("upsert peer");

    let response = state.handle_machine_rtt().await;

    assert!(response.ok);
    let Some(DaemonPayload::MachineRtt(payload)) = response.payload else {
        panic!("expected machine rtt payload");
    };
    assert!(payload.rows.is_empty());
    assert!(payload.warnings.is_empty());
    assert_eq!(response.message, "no rtt samples");

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_activates_joiner_lifecycle() {
    let _guard = test_ssh_env_lock().lock().await;
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("bind overlay listener");
    let peer_rpc_port = listener.local_addr().expect("listener addr").port();
    let remote_control_port = peer_rpc_port
        .checked_sub(1)
        .expect("peer rpc port has preceding remote control port");
    let (mut state, store, network) = make_state_with_remote_port(true, remote_control_port).await;
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

    let server_store = store.clone();
    let server = tokio::spawn(async move {
        for _ in 0..5 {
            let (stream, _) = listener.accept().await.expect("accept overlay rpc");
            let (reader, mut writer) = stream.into_split();
            let mut buf = BufReader::new(reader);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            let request: DaemonRequest =
                serde_json::from_str(&line).expect("decode daemon request");
            let response = if matches!(request, DaemonRequest::Coord { .. }) {
                DaemonResponse {
                    ok: true,
                    code: "OK".into(),
                    message: "allow".into(),
                    payload: None,
                }
            } else if matches!(request, DaemonRequest::MeshReady { .. }) {
                DaemonResponse {
                    ok: true,
                    code: "OK".into(),
                    message: "ready".into(),
                    payload: Some(DaemonPayload::MeshReady(MeshReadyPayload {
                        ready: true,
                        phase: "running".into(),
                        store_healthy: true,
                        sync_connected: true,
                        workload_subnet_present: true,
                    })),
                }
            } else if let DaemonRequest::MachineTransitionSelf {
                goal: MachineTransitionGoal::Activate,
                assigned_subnet,
                ..
            } = request
            {
                {
                    let mut joiner_record = MachineMembership::seed(
                        MachineId("joiner-1".into()),
                        PublicKey([4; 32]),
                        "::1".parse().map(OverlayIp).expect("valid overlay"),
                        Some("10.210.99.0/24".parse().expect("valid subnet")),
                        vec!["203.0.113.10:51820".into()],
                    );
                    joiner_record.subnet = assigned_subnet;
                    joiner_record.lifecycle = MachineLifecycle::Active;
                    server_store
                        .upsert_self_machine(&joiner_record)
                        .await
                        .expect("upsert joiner for projection");
                    DaemonResponse {
                        ok: true,
                        code: "OK".into(),
                        message: "enabled".into(),
                        payload: None,
                    }
                }
            } else {
                panic!("unexpected daemon request: {request:?}");
            };
            let mut response_line = serde_json::to_string(&response).expect("encode response");
            response_line.push('\n');
            writer
                .write_all(response_line.as_bytes())
                .await
                .expect("write response");
            writer.shutdown().await.expect("shutdown writer");
        }
    });

    let mut reserved = state
        .reserve_machine_subnet(&MachineId("join-target".into()))
        .await
        .expect("reserve expected subnet");
    let expected_subnet = reserved.subnet();
    release_reserved_subnet(&mut reserved)
        .await
        .expect("release reserved subnet for re-acquire");

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-1".into()),
        public_key: PublicKey([4; 32]),
        overlay_ip: "::1".parse().map(OverlayIp).expect("valid overlay"),
        topology: MachineTopology::local(),
        role: MachineRole::Mirror,
        subnet: Some(expected_subnet),
        endpoints: vec!["203.0.113.10:51820".into()],
    }
    .encode()
    .expect("encode join response");

    let ssh_dir = unique_temp_dir("ployz-fake-ssh");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let self_record_response = serde_json::to_string(&DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: join_response.clone(),
        payload: Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            encoded: join_response.clone(),
            record: JoinResponse::decode(&join_response)
                .expect("decode join response")
                .into_seed_machine_membership(),
        })),
    })
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _status_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_STATUS_RESPONSE",
        Some(status_response_for_join_response(&join_response).into()),
    );
    let _ready_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_READY_RESPONSE",
        Some(
            "{\"ok\":true,\"code\":\"OK\",\"message\":\"ready\",\"payload\":{\"kind\":\"mesh-ready\",\"ready\":true,\"phase\":\"running\",\"store_healthy\":true,\"sync_connected\":true,\"workload_subnet_present\":true}}".into(),
        ),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(response.ok, "{}", response.message);
    assert!(response.message.contains("awaiting_self_publication: 1"));

    let joiner = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id.0 == "joiner-1")
        .expect("joiner should be in store after enable");
    assert_eq!(joiner.lifecycle, MachineLifecycle::Active);
    assert!(
        network
            .current_peers()
            .into_iter()
            .any(|peer| peer.id().0 == "joiner-1")
    );
    server.await.expect("overlay server exit");

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_requires_sync_connected_for_running_joiner() {
    let _guard = test_ssh_env_lock().lock().await;
    let (mut state, store, network) = make_state(true).await;

    let mut reserved = state
        .reserve_machine_subnet(&MachineId("join-target".into()))
        .await
        .expect("reserve expected subnet");
    let expected_subnet = reserved.subnet();
    release_reserved_subnet(&mut reserved)
        .await
        .expect("release reserved subnet for re-acquire");

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-2".into()),
        public_key: PublicKey([5; 32]),
        overlay_ip: "fd00::5".parse().map(OverlayIp).expect("valid overlay"),
        topology: MachineTopology::local(),
        role: MachineRole::Mirror,
        subnet: Some(expected_subnet),
        endpoints: vec!["203.0.113.11:51820".into()],
    }
    .encode()
    .expect("encode join response");

    let ssh_dir = unique_temp_dir("ployz-fake-ssh");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let self_record_response = serde_json::to_string(&DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: join_response.clone(),
        payload: Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            encoded: join_response.clone(),
            record: JoinResponse::decode(&join_response)
                .expect("decode join response")
                .into_seed_machine_membership(),
        })),
    })
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _status_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_STATUS_RESPONSE",
        Some(status_response_for_join_response(&join_response).into()),
    );
    let _ready_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_READY_RESPONSE",
        Some(
            "{\"ok\":true,\"code\":\"OK\",\"message\":\"ready\",\"payload\":{\"kind\":\"mesh-ready\",\"ready\":false,\"phase\":\"running\",\"store_healthy\":true,\"sync_connected\":false,\"workload_subnet_present\":true}}".into(),
        ),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.ok, "{}", response.message);
    assert_eq!(response.code, "MACHINE_ADD_FAILED");
    assert!(response.message.contains("failed_ready: 1"));
    assert!(
        response
            .message
            .contains("timed out waiting for remote mesh readiness")
    );

    let machines = store.list_machines().await.expect("list machines");
    assert!(
        !machines
            .into_iter()
            .any(|machine| machine.id.0 == "joiner-2")
    );
    assert!(
        !network
            .current_peers()
            .into_iter()
            .any(|peer| peer.id().0 == "joiner-2")
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
    assert!(!response.ok);
    assert_eq!(response.code, "MACHINE_ADD_FAILED");
    assert!(response.message.contains("create machine operations dir"));

    let invites = store.list_invites().await.expect("list invites");
    assert_eq!(invites.len(), 1);
    assert!(
        invites
            .first()
            .expect("invite created")
            .consumed_by
            .is_none()
    );
    assert!(network.current_peers().is_empty());

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn subnet_reservation_skips_currently_held_candidate() {
    let (mut state, _, _) = make_state(true).await;

    let mut first = state
        .reserve_machine_subnet(&MachineId("join-target".into()))
        .await
        .expect("first reservation");
    let first_subnet = first.subnet();
    let mut second = state
        .reserve_machine_subnet(&MachineId("join-target".into()))
        .await
        .expect("second reservation should skip held candidate");

    assert_ne!(first_subnet, second.subnet());

    release_reserved_subnet(&mut second)
        .await
        .expect("release second");
    release_reserved_subnet(&mut first)
        .await
        .expect("release first");
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
    assert!(!response.ok);
    assert!(response.message.contains("--force"));
}

#[tokio::test]
async fn machine_remove_reports_peer_rejection_without_unreachable_hint() {
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("bind overlay listener");
    let peer_rpc_port = listener.local_addr().expect("listener addr").port();
    let remote_control_port = peer_rpc_port
        .checked_sub(1)
        .expect("peer rpc port has preceding remote control port");
    let (mut state, store, _) = make_state_with_remote_port(true, remote_control_port).await;

    let mut peer = test_machine_record(
        "peer-1",
        "10.210.1.0/24",
        MachineLifecycle::Active,
        PublicKey([2; 32]),
    );
    peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    store.upsert_self_machine(&peer).await.expect("upsert peer");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept overlay rpc");
        let (reader, mut writer) = stream.into_split();
        let mut buf = BufReader::new(reader);
        let mut line = String::new();
        buf.read_line(&mut line).await.expect("read request");
        let request: DaemonRequest = serde_json::from_str(&line).expect("decode daemon request");
        assert!(matches!(
            request,
            DaemonRequest::MeshPeerRemoveMachine { .. }
        ));
        let mut response_line = serde_json::to_string(&DaemonResponse {
            ok: false,
            code: "MACHINE_REMOVE_FAILED".into(),
            message: "remote cleanup rejected".into(),
            payload: None,
        })
        .expect("encode response");
        response_line.push('\n');
        writer
            .write_all(response_line.as_bytes())
            .await
            .expect("write response");
        writer.shutdown().await.expect("shutdown writer");
    });

    let response = state.handle_machine_remove("peer-1", false).await;
    assert!(!response.ok);
    assert_eq!(response.code, "MACHINE_REMOVE_PEER_REJECTED");
    assert!(response.message.contains("MACHINE_REMOVE_FAILED"));
    assert!(!response.message.contains("did not confirm online removal"));

    server.await.expect("overlay server exit");
    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_remove_waits_for_long_running_peer_teardown() {
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("bind overlay listener");
    let peer_rpc_port = listener.local_addr().expect("listener addr").port();
    let remote_control_port = peer_rpc_port
        .checked_sub(1)
        .expect("peer rpc port has preceding remote control port");
    let (mut state, store, _) = make_state_with_remote_port(true, remote_control_port).await;

    let mut peer = test_machine_record(
        "peer-1",
        "10.210.1.0/24",
        MachineLifecycle::Active,
        PublicKey([2; 32]),
    );
    peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    store.upsert_self_machine(&peer).await.expect("upsert peer");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept overlay rpc");
        let (reader, mut writer) = stream.into_split();
        let mut buf = BufReader::new(reader);
        let mut line = String::new();
        buf.read_line(&mut line).await.expect("read request");
        let request: DaemonRequest = serde_json::from_str(&line).expect("decode daemon request");
        assert!(matches!(
            request,
            DaemonRequest::MeshPeerRemoveMachine { .. }
        ));
        sleep(Duration::from_secs(4)).await;
        let mut response_line = serde_json::to_string(&DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "removed".into(),
            payload: None,
        })
        .expect("encode response");
        response_line.push('\n');
        writer
            .write_all(response_line.as_bytes())
            .await
            .expect("write response");
        writer.shutdown().await.expect("shutdown writer");
    });

    let response = state.handle_machine_remove("peer-1", false).await;
    assert!(response.ok, "{}", response.message);

    let machines = store.list_machines().await.expect("list machines");
    assert!(!machines.into_iter().any(|machine| machine.id.0 == "peer-1"));

    server.await.expect("overlay server exit");
    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_remove_force_deletes_registry_record() {
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
    assert!(response.ok, "{}", response.message);

    let machines = store.list_machines().await.expect("list machines");
    assert!(!machines.into_iter().any(|machine| machine.id.0 == "peer-1"));
}

#[tokio::test]
async fn local_standby_clears_subnet_and_marks_standby() {
    let (mut state, store, _) = make_state(true).await;
    let response = state
        .transition_local_machine(MachineTransitionGoal::Standby, None, true)
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
        .transition_local_machine(MachineTransitionGoal::Standby, None, false)
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
async fn local_standby_restores_subnet_when_restart_fails() {
    let (mut state, _, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let original_config = NetworkConfig::load(&config_path).expect("load original config");
    state.gateway_listen_addr = "invalid".into();

    let response = state
        .transition_local_machine(MachineTransitionGoal::Standby, None, true)
        .await;
    assert!(response.is_err());

    let restored_config = NetworkConfig::load(&config_path).expect("load restored config");
    assert_eq!(restored_config.subnet, original_config.subnet);
}

#[tokio::test]
async fn mesh_start_reactivates_local_machine_after_stop() {
    let (mut state, _, _) = make_state(true).await;

    let standby = state
        .transition_local_machine(MachineTransitionGoal::Standby, None, true)
        .await;
    assert!(standby.is_ok(), "{standby:?}");

    let down = state.handle_mesh_stop(true).await;
    assert!(down.ok, "{}", down.message);

    let up = state.handle_mesh_start("alpha").await;
    assert!(up.ok, "{}", up.message);

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
        .transition_local_machine(
            MachineTransitionGoal::Activate,
            Some("10.210.0.0/24".parse().expect("valid subnet")),
            false,
        )
        .await;
    assert!(activate.is_ok(), "{activate:?}");

    network.set_fail_down(true);

    let response = state.handle_mesh_stop(true).await;
    assert!(!response.ok);
    assert_eq!(response.code, "NETWORK_STOP_FAILED");
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
    assert!(!response.ok, "{}", response.message);
    assert_eq!(response.code, "MACHINE_ADD_FAILED");
}

#[tokio::test]
async fn machine_add_rejects_remote_subnet_mismatch_before_invite_consume() {
    let _guard = test_ssh_env_lock().lock().await;
    let (state, store, _) = make_state(true).await;

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-mismatch".into()),
        public_key: PublicKey([14; 32]),
        overlay_ip: "fd00::14".parse().map(OverlayIp).expect("valid overlay"),
        topology: MachineTopology::local(),
        role: MachineRole::Mirror,
        subnet: Some("10.210.99.0/24".parse().expect("valid subnet")),
        endpoints: vec!["203.0.113.14:51820".into()],
    }
    .encode()
    .expect("encode join response");

    let ssh_dir = unique_temp_dir("ployz-fake-ssh-mismatch");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh.clone());
    let _ployz_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_LOCAL_PLOYZCTL",
        Some(fake_ssh.clone().into_os_string()),
    );
    let self_record_response = serde_json::to_string(&DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: join_response.clone(),
        payload: Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            encoded: join_response.clone(),
            record: JoinResponse::decode(&join_response)
                .expect("decode join response")
                .into_seed_machine_membership(),
        })),
    })
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _status_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_STATUS_RESPONSE",
        Some(status_response_for_join_response(&join_response).into()),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.ok, "{}", response.message);
    assert_eq!(response.code, "MACHINE_ADD_FAILED");
    assert!(response.message.contains("founder reserved"));

    let invites = store.list_invites().await.expect("list invites");
    assert_eq!(invites.len(), 1);
    assert_eq!(invites.first().expect("invite created").consumed_by, None);
}

#[tokio::test]
async fn local_activate_restores_subnet_before_active_finalization() {
    let (mut state, store, _) = make_state(true).await;
    let standby_response = state
        .transition_local_machine(MachineTransitionGoal::Standby, None, true)
        .await;
    assert!(standby_response.is_ok(), "{standby_response:?}");

    let activate_response = state
        .transition_local_machine(
            MachineTransitionGoal::Activate,
            Some("10.210.2.0/24".parse().expect("valid subnet")),
            false,
        )
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
async fn machine_activate_rolls_back_remote_activate_when_self_record_fails() {
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("bind overlay listener");
    let peer_rpc_port = listener.local_addr().expect("listener addr").port();
    let remote_control_port = peer_rpc_port
        .checked_sub(1)
        .expect("peer rpc port has preceding remote control port");
    let (mut state, store, _) = make_state_with_remote_port(true, remote_control_port).await;

    let mut peer = test_machine_record(
        "peer",
        "10.210.1.0/24",
        MachineLifecycle::Standby,
        PublicKey([7; 32]),
    );
    peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    peer.subnet = None;
    store.upsert_self_machine(&peer).await.expect("upsert peer");

    let seen_requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let server_seen_requests = Arc::clone(&seen_requests);
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (stream, _) = listener.accept().await.expect("accept overlay rpc");
            let (reader, mut writer) = stream.into_split();
            let mut buf = BufReader::new(reader);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            let request: DaemonRequest =
                serde_json::from_str(&line).expect("decode daemon request");
            let (label, response) = if let DaemonRequest::MachineTransitionSelf {
                goal: MachineTransitionGoal::Activate,
                ..
            } = request
            {
                (
                    "activate",
                    DaemonResponse {
                        ok: true,
                        code: "OK".into(),
                        message: "activated".into(),
                        payload: None,
                    },
                )
            } else if matches!(request, DaemonRequest::MeshSelfRecord) {
                (
                    "self_record",
                    DaemonResponse {
                        ok: false,
                        code: "BROKEN_SELF_RECORD".into(),
                        message: "self record unavailable".into(),
                        payload: None,
                    },
                )
            } else if let DaemonRequest::MachineTransitionSelf {
                goal: MachineTransitionGoal::Standby,
                force,
                ..
            } = request
            {
                {
                    assert!(force);
                    (
                        "standby",
                        DaemonResponse {
                            ok: true,
                            code: "OK".into(),
                            message: "standby".into(),
                            payload: None,
                        },
                    )
                }
            } else {
                panic!("unexpected daemon request: {request:?}");
            };
            server_seen_requests.lock().await.push(label.into());
            let mut response_line = serde_json::to_string(&response).expect("encode response");
            response_line.push('\n');
            writer
                .write_all(response_line.as_bytes())
                .await
                .expect("write response");
            writer.shutdown().await.expect("shutdown writer");
        }
    });

    let response = state.handle_machine_activate("peer").await;
    assert!(!response.ok, "{}", response.message);
    assert_eq!(response.code, "SELF_RECORD_FAILED");

    server.await.expect("overlay server exit");
    let seen_requests = seen_requests.lock().await.clone();
    assert_eq!(
        seen_requests,
        vec![
            "activate".to_string(),
            "self_record".to_string(),
            "standby".to_string()
        ]
    );

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn interrupted_machine_add_is_marked_interrupted_on_startup() {
    let (state, _, _) = make_state(false).await;
    let store = state.machine_operation_store();
    let mut operation = store
        .begin(
            MachineOperationKind::Add,
            Some("alpha".into()),
            vec!["join-target".into()],
            "transient-peer-installed",
            MachineOperationArtifacts {
                machine_id: Some(MachineId("joiner-1".into())),
                invite_id: Some("invite-1".into()),
                allocated_subnet: Some("10.210.2.0/24".into()),
                ..MachineOperationArtifacts::default()
            },
        )
        .expect("begin operation");
    store
        .update_status(&mut operation, MachineOperationStatus::Running, None)
        .expect("keep running");

    state.reconcile_machine_operations_on_startup().await;

    let reconciled = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(reconciled.status, MachineOperationStatus::Interrupted);
    assert!(
        reconciled
            .last_error
            .as_deref()
            .expect("last error")
            .contains("daemon restarted")
    );
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

    state.reconcile_machine_operations_on_startup().await;

    let reconciled = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(reconciled.status, MachineOperationStatus::Interrupted);
    assert!(
        reconciled
            .last_error
            .as_deref()
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

    state.reconcile_machine_operations_on_startup().await;

    let reconciled = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(reconciled.status, MachineOperationStatus::Succeeded);
    assert!(reconciled.last_error.is_none());
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

    state.reconcile_machine_operations_on_startup().await;

    let reconciled = state
        .machine_operation_store()
        .load(&operation.id)
        .expect("load operation")
        .expect("operation exists");
    assert_eq!(reconciled.status, MachineOperationStatus::Interrupted);
    assert!(
        reconciled
            .last_error
            .as_deref()
            .expect("last error")
            .contains("expected 999.999.999")
    );
}

async fn make_state(start_mesh: bool) -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
    make_state_with_remote_port(start_mesh, 4317).await
}

async fn make_state_with_remote_port(
    start_mesh: bool,
    remote_control_port: u16,
) -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
    let identity = Identity::generate(MachineId("founder".into()), [1; 32]);
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
        MachineLifecycle::Standby,
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
        remote_control_port,
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
    let cached_subnet = config.subnet;
    state.active = Some(ActiveMesh {
        config,
        cached_subnet,
        mesh,
        remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        peer_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        certificate_renewal: None,
        bootstrap_seed_cache: None,
    });

    (state, store, network)
}

async fn teardown_state(state: &mut DaemonState) {
    let Some(active) = state.active.as_mut() else {
        return;
    };
    active.mesh.destroy().await.expect("destroy mesh");
}

fn test_machine_record(
    id: &str,
    subnet: &str,
    lifecycle: MachineLifecycle,
    public_key: PublicKey,
) -> MachineMembership {
    MachineMembership {
        id: MachineId(id.into()),
        public_key,
        overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
            .parse()
            .map(OverlayIp)
            .expect("valid overlay"),
        topology: MachineTopology::local(),
        control_target: Some(id.into()),
        subnet: Some(subnet.parse().expect("valid subnet")),
        bridge_ip: None,
        endpoints: vec!["127.0.0.1:51820".into()],
        lifecycle,
        role: MachineRole::StorageCandidate,
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

fn status_response_for_join_response(join_response: &str) -> String {
    let record = JoinResponse::decode(join_response)
        .expect("decode join response")
        .into_seed_machine_membership();
    serde_json::to_string(&DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: "status".into(),
        payload: Some(DaemonPayload::Status(StatusPayload {
            machine_id: record.id.0,
            public_key: record.public_key,
            version: "test-version".into(),
            network: None,
            overlay_ip: None,
            network_lifecycle: None,
            local_machine_lifecycle: None,
            mesh_phase: "idle".into(),
        })),
    })
    .expect("encode status response")
}

fn write_fake_ssh(dir: &Path) -> PathBuf {
    let script = dir.join("ssh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprev=''\nfor arg in \"$@\"; do\n  target=\"$prev\"\n  command=\"$arg\"\n  prev=\"$arg\"\ndone\nif [ \"$command\" = 'set -eu; \"$HOME/.local/bin/ployzctl\" rpc-stdio' ]; then\n  req=$(cat)\n  case \"$req\" in\n    *'\"Coord\"'*)\n      case \"$req\" in\n        *'\"Prepare\"'*)\n          case \",$PLOYZ_TEST_COORD_PREPARE_DENY_TARGETS,\" in\n            *\",$target,\"*) printf '{\"ok\":false,\"code\":\"COORDINATION_DENIED\",\"message\":\"denied\",\"payload\":null}' ;;\n            *) printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"allow\",\"payload\":null}' ;;\n          esac\n          ;;\n        *'\"Release\"'*)\n          case \",$PLOYZ_TEST_COORD_RELEASE_DENY_TARGETS,\" in\n            *\",$target,\"*) printf '{\"ok\":false,\"code\":\"COORDINATION_DENIED\",\"message\":\"denied\",\"payload\":null}' ;;\n            *) printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"allow\",\"payload\":null}' ;;\n          esac\n          ;;\n        *)\n          printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"ack\",\"payload\":null}'\n          ;;\n      esac\n      ;;\n    *'\"Status\"'*)\n      printf '%s' \"$PLOYZ_TEST_STATUS_RESPONSE\"\n      ;;\n    *'\"MeshBootstrap\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"bootstrapped\",\"payload\":null}'\n      ;;\n    *'\"MeshJoin\"'*)\n      printf '{\"ok\":false,\"code\":\"UNSUPPORTED\",\"message\":\"unsupported\",\"payload\":null}'\n      ;;\n    *'\"MeshInit\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"init\",\"payload\":null}'\n      ;;\n    *'\"MeshDestroy\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"destroyed\",\"payload\":null}'\n      ;;\n    *'\"MeshDown\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"down\",\"payload\":null}'\n      ;;\n    *'\"MeshSelfRecord\"'*)\n      printf '%s' \"$PLOYZ_TEST_SELF_RECORD_RESPONSE\"\n      ;;\n    *'\"MeshReady\"'*)\n      printf '%s' \"$PLOYZ_TEST_READY_RESPONSE\"\n      ;;\n    *)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"ok\",\"payload\":null}'\n      ;;\n  esac\n  exit 0\nfi\ncase \"$command\" in\n  *'--version'*)\n    printf 'ployzctl test-version'\n    exit 0\n    ;;\n  *'status >/dev/null'*)\n    case \",$PLOYZ_TEST_STATUS_FAIL_TARGETS,\" in\n      *\",$target,\"*) exit 1 ;;\n      *) exit 0 ;;\n    esac\n    ;;\n  *'bash -s -- install'*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
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
