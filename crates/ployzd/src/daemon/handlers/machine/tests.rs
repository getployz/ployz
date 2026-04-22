use super::heal::plan_local_subnet_heal;
use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use crate::daemon::ActiveMesh;
use crate::daemon::DaemonState;
use crate::daemon::ssh::{TestSshEnvGuard, TestSshProgramGuard, test_ssh_env_lock};
use crate::mesh_state::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
use ipnet::Ipv4Net;
use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineAddOptions, MeshSelfRecordPayload,
};
use ployz_orchestrator::Mesh;
use ployz_orchestrator::mesh::driver::WireguardDriver;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_runtime_api::Identity;
use ployz_store_api::StoreDriver;
use ployz_store_api::memory::{MemoryService, MemoryStore};
use ployz_store_api::{InviteStore, MachineStore};
use ployz_types::model::{
    JoinResponse, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
};
use ployz_types::time::now_unix_secs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn machine_list_shows_disabled_explicitly() {
    let (state, store, _) = make_state(false).await;
    let disabled = test_machine_record(
        "peer-disabled",
        "10.210.1.0/24",
        Participation::Disabled,
        0,
        PublicKey([2; 32]),
    );
    store
        .upsert_self_machine(&disabled)
        .await
        .expect("upsert disabled peer");

    let response = state.handle_machine_list().await;
    assert!(response.ok);
    assert!(response.message.contains("LIVENESS"));
    assert!(response.message.contains("peer-disabled"));
    assert!(response.message.contains("disabled"));
    assert!(response.message.contains("stale"));
}

#[tokio::test]
async fn machine_list_shows_down_liveness() {
    let (state, store, _) = make_state(false).await;
    let mut down = test_machine_record(
        "peer-down",
        "10.210.1.0/24",
        Participation::Enabled,
        now_unix_secs(),
        PublicKey([2; 32]),
    );
    down.status = MachineStatus::Down;
    store
        .upsert_self_machine(&down)
        .await
        .expect("upsert down peer");

    let response = state.handle_machine_list().await;
    assert!(response.ok);
    assert!(response.message.contains("peer-down"));
    assert!(response.message.contains("down"));
}

#[tokio::test]
async fn machine_list_json_payload_contains_rows() {
    let (state, _, _) = make_state(false).await;
    let response = state.handle_machine_list().await;
    let Some(DaemonPayload::MachineList(payload)) = response.payload else {
        panic!("expected machine list payload");
    };
    assert_eq!(payload.rows.len(), 1);
    assert_eq!(payload.rows[0].id, "founder");
}

#[test]
fn plan_local_subnet_heal_reassigns_losing_machine() {
    let machines = vec![
        test_machine_record(
            "alpha",
            "10.210.0.0/24",
            Participation::Enabled,
            0,
            PublicKey([2; 32]),
        ),
        test_machine_record(
            "beta",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([3; 32]),
        ),
        test_machine_record(
            "gamma",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([4; 32]),
        ),
    ];

    let plan = plan_local_subnet_heal(
        &machines,
        &MachineId("gamma".into()),
        DEFAULT_CLUSTER_CIDR,
        24,
    )
    .expect("plan should succeed")
    .expect("gamma should heal");

    assert_eq!(plan.current_subnet, "10.210.1.0/24".parse().expect("valid"));
    assert_eq!(plan.winner_machine_id, MachineId("beta".into()));
    assert_eq!(plan.target_subnet, "10.210.2.0/24".parse().expect("valid"));
}

#[test]
fn plan_local_subnet_heal_keeps_winner_in_place() {
    let machines = vec![
        test_machine_record(
            "alpha",
            "10.210.0.0/24",
            Participation::Enabled,
            0,
            PublicKey([2; 32]),
        ),
        test_machine_record(
            "beta",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([3; 32]),
        ),
        test_machine_record(
            "gamma",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([4; 32]),
        ),
    ];

    let plan = plan_local_subnet_heal(
        &machines,
        &MachineId("beta".into()),
        DEFAULT_CLUSTER_CIDR,
        24,
    )
    .expect("plan should succeed");

    assert!(plan.is_none());
}

#[test]
fn plan_local_subnet_heal_is_noop_after_subnet_changes() {
    let machines = vec![
        test_machine_record(
            "alpha",
            "10.210.0.0/24",
            Participation::Enabled,
            0,
            PublicKey([2; 32]),
        ),
        test_machine_record(
            "beta",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([3; 32]),
        ),
        test_machine_record(
            "gamma",
            "10.210.2.0/24",
            Participation::Enabled,
            0,
            PublicKey([4; 32]),
        ),
    ];

    let plan = plan_local_subnet_heal(
        &machines,
        &MachineId("gamma".into()),
        DEFAULT_CLUSTER_CIDR,
        24,
    )
    .expect("plan should succeed");

    assert!(plan.is_none());
}

#[tokio::test]
async fn machine_add_warns_on_degraded_mesh_and_publishes_disabled_joiner() {
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
        Participation::Enabled,
        0,
        PublicKey([3; 32]),
    );
    stale_peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    stale_peer.status = MachineStatus::Up;
    store
        .upsert_self_machine(&stale_peer)
        .await
        .expect("upsert stale peer");

    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.expect("accept overlay rpc");
            let (reader, mut writer) = stream.into_split();
            let mut buf = BufReader::new(reader);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            let request: DaemonRequest =
                serde_json::from_str(&line).expect("decode daemon request");
            match request {
                DaemonRequest::Coord { .. } => {}
                other => panic!("unexpected daemon request: {other:?}"),
            }
            let mut response_line = serde_json::to_string(&DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "allow".into(),
                payload: None,
            })
            .expect("encode response");
            response_line.push('\n');
            writer
                .write_all(response_line.as_bytes())
                .await
                .expect("write response");
            writer.shutdown().await.expect("shutdown writer");
        }
    });

    let reserved = state
        .reserve_machine_subnet(&MachineId("join-target".into()))
        .await
        .expect("reserve expected subnet");
    let _ = state
        .reservations
        .release(
            reserved.reservation_key(),
            reserved.reservation_nonce(),
            now_unix_secs(),
        )
        .await;
    let expected_subnet = reserved.subnet();

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-1".into()),
        public_key: PublicKey([4; 32]),
        overlay_ip: "fd00::4".parse().map(OverlayIp).expect("valid overlay"),
        subnet: Some(expected_subnet),
        endpoints: vec!["203.0.113.10:51820".into()],
    }
    .encode()
    .expect("encode join response");

    let ssh_dir = unique_temp_dir("ployz-fake-ssh");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
    let self_record_response = serde_json::to_string(&DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: join_response.clone(),
        payload: Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            encoded: join_response.clone(),
            record: JoinResponse::decode(&join_response)
                .expect("decode join response")
                .into_seed_machine_record(),
        })),
    })
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _ready_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_READY_RESPONSE",
        Some(
            "{\"ok\":true,\"code\":\"OK\",\"message\":\"ready\",\"payload\":{\"kind\":\"mesh-ready\",\"ready\":true,\"phase\":\"running\",\"store_healthy\":true,\"sync_connected\":true,\"heartbeat_started\":true}}".into(),
        ),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(response.ok, "{}", response.message);
    assert!(
        response
            .message
            .contains("warning: enabled peer 'stale-peer' has a stale heartbeat")
    );
    assert!(response.message.contains("awaiting_self_publication: 1"));

    let machines = store.list_machines().await.expect("list machines");
    assert!(
        !machines
            .into_iter()
            .any(|machine| machine.id.0 == "joiner-1")
    );
    assert!(
        network
            .current_peers()
            .into_iter()
            .any(|machine| machine.id.0 == "joiner-1")
    );
    server.await.expect("overlay server exit");

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_accepts_running_joiner_before_full_sync() {
    let _guard = test_ssh_env_lock().lock().await;
    let (mut state, store, network) = make_state(true).await;

    let reserved = state
        .reserve_machine_subnet(&MachineId("join-target".into()))
        .await
        .expect("reserve expected subnet");
    let _ = state
        .reservations
        .release(
            reserved.reservation_key(),
            reserved.reservation_nonce(),
            now_unix_secs(),
        )
        .await;
    let expected_subnet = reserved.subnet();

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-2".into()),
        public_key: PublicKey([5; 32]),
        overlay_ip: "fd00::5".parse().map(OverlayIp).expect("valid overlay"),
        subnet: Some(expected_subnet),
        endpoints: vec!["203.0.113.11:51820".into()],
    }
    .encode()
    .expect("encode join response");

    let ssh_dir = unique_temp_dir("ployz-fake-ssh");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
    let self_record_response = serde_json::to_string(&DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: join_response.clone(),
        payload: Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            encoded: join_response.clone(),
            record: JoinResponse::decode(&join_response)
                .expect("decode join response")
                .into_seed_machine_record(),
        })),
    })
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );
    let _ready_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_READY_RESPONSE",
        Some(
            "{\"ok\":true,\"code\":\"OK\",\"message\":\"ready\",\"payload\":{\"kind\":\"mesh-ready\",\"ready\":false,\"phase\":\"running\",\"store_healthy\":true,\"sync_connected\":false,\"heartbeat_started\":true}}".into(),
        ),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(response.ok, "{}", response.message);
    assert!(response.message.contains("awaiting_self_publication: 1"));

    let machines = store.list_machines().await.expect("list machines");
    assert!(
        !machines
            .into_iter()
            .any(|machine| machine.id.0 == "joiner-2")
    );
    assert!(
        network
            .current_peers()
            .into_iter()
            .any(|machine| machine.id.0 == "joiner-2")
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
    assert!(
        state
            .reservations
            .active_subnets(now_unix_secs())
            .await
            .is_empty()
    );

    let invites = store.list_invites().await.expect("list invites");
    assert_eq!(invites.len(), 1);
    assert!(invites[0].consumed_by.is_none());
    assert!(network.current_peers().is_empty());

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_remove_refuses_enabled_without_force() {
    let (state, store, _) = make_state(false).await;
    store
        .upsert_self_machine(&test_machine_record(
            "peer-1",
            "10.210.1.0/24",
            Participation::Enabled,
            10,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert peer");

    let response = state.handle_machine_remove("peer-1", false).await;
    assert!(!response.ok);
    assert!(response.message.contains("must be disabled"));
}

#[tokio::test]
async fn machine_remove_deletes_disabled_record() {
    let (state, store, _) = make_state(false).await;
    store
        .upsert_self_machine(&test_machine_record(
            "peer-1",
            "10.210.1.0/24",
            Participation::Disabled,
            10,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert peer");

    let response = state.handle_machine_remove("peer-1", false).await;
    assert!(response.ok, "{}", response.message);

    let machines = store.list_machines().await.expect("list machines");
    assert!(!machines.into_iter().any(|machine| machine.id.0 == "peer-1"));
}

#[tokio::test]
async fn mesh_standby_clears_subnet_and_marks_disabled() {
    let (mut state, store, _) = make_state(true).await;
    let response = state.handle_mesh_standby(true).await;
    assert!(response.ok, "{}", response.message);

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.participation, Participation::Disabled);
    assert_eq!(local.subnet, None);
    assert_eq!(local.status, MachineStatus::Up);
    assert_eq!(
        state.active.as_ref().expect("active mesh").config.subnet,
        None
    );
}

#[tokio::test]
async fn mesh_standby_auto_drains_when_no_local_workloads_exist() {
    let (mut state, store, _) = make_state(true).await;
    {
        let active = state.active.as_mut().expect("active mesh");
        let Some(mut record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.participation = Participation::Enabled;
                record.status = MachineStatus::Up;
                record.subnet = Some("10.210.0.0/24".parse().expect("valid subnet"));
            })
            .await
        else {
            panic!("self record missing");
        };
        record.participation = Participation::Enabled;
        record.status = MachineStatus::Up;
        record.subnet = Some("10.210.0.0/24".parse().expect("valid subnet"));
        active
            .mesh
            .store
            .upsert_self_machine(&record)
            .await
            .expect("persist enabled self");
    }

    let response = state.handle_mesh_standby(false).await;
    assert!(response.ok, "{}", response.message);

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.participation, Participation::Disabled);
    assert_eq!(local.subnet, None);
}

#[tokio::test]
async fn mesh_standby_restores_subnet_when_restart_fails() {
    let (mut state, _, _) = make_state(true).await;
    let config_path = NetworkConfig::path(&state.data_dir, "alpha");
    let original_config = NetworkConfig::load(&config_path).expect("load original config");
    state.gateway_listen_addr = "invalid".into();

    let response = state.handle_mesh_standby(true).await;
    assert!(!response.ok);
    assert_eq!(response.code, "NETWORK_RESTART_FAILED");

    let restored_config = NetworkConfig::load(&config_path).expect("load restored config");
    assert_eq!(restored_config.subnet, original_config.subnet);
}

#[tokio::test]
async fn reserve_machine_subnet_clears_local_hold_when_quorum_denies() {
    let _guard = test_ssh_env_lock().lock().await;
    let (mut state, store, _) = make_state(true).await;
    state.cluster_cidr = "10.210.0.0/22".into();
    store
        .upsert_self_machine(&test_machine_record(
            "peer-quorum",
            "10.210.1.0/24",
            Participation::Enabled,
            now_unix_secs(),
            PublicKey([6; 32]),
        ))
        .await
        .expect("upsert quorum peer");

    let ssh_dir = unique_temp_dir("ployz-fake-ssh-quorum-deny");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
    let _deny_prepare_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_COORD_PREPARE_DENY_TARGETS",
        Some("peer-quorum".into()),
    );

    let result = state
        .reserve_machine_subnet(&MachineId("joiner".into()))
        .await;
    assert!(result.is_err());
    assert!(
        state
            .reservations
            .active_subnets(now_unix_secs())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn reserve_machine_subnet_releases_peer_holds_when_quorum_denies() {
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("bind overlay listener");
    let peer_rpc_port = listener.local_addr().expect("listener addr").port();
    let remote_control_port = peer_rpc_port
        .checked_sub(1)
        .expect("peer rpc port has preceding remote control port");
    let (mut state, store, _) = make_state_with_remote_port(true, remote_control_port).await;
    state.cluster_cidr = "10.210.0.0/22".into();

    for (name, key_seed) in [("peer-a", 11u8), ("peer-b", 12u8), ("peer-c", 13u8)] {
        let mut peer = test_machine_record(
            name,
            "10.210.1.0/24",
            Participation::Enabled,
            now_unix_secs(),
            PublicKey([key_seed; 32]),
        );
        peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
        peer.subnet = None;
        peer.status = MachineStatus::Up;
        store
            .upsert_self_machine(&peer)
            .await
            .expect("upsert quorum peer");
    }

    let seen_requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let server_seen_requests = Arc::clone(&seen_requests);
    let server = tokio::spawn(async move {
        for index in 0..6 {
            let (stream, _) = listener.accept().await.expect("accept overlay rpc");
            let (reader, mut writer) = stream.into_split();
            let mut buf = BufReader::new(reader);
            let mut line = String::new();
            buf.read_line(&mut line).await.expect("read request");
            let request: DaemonRequest =
                serde_json::from_str(&line).expect("decode daemon request");
            let (label, response) = match request {
                DaemonRequest::Coord {
                    op: ployz_api::CoordOp::Prepare { .. },
                } => {
                    let response = if index == 0 {
                        DaemonResponse {
                            ok: true,
                            code: "OK".into(),
                            message: "allow".into(),
                            payload: None,
                        }
                    } else {
                        DaemonResponse {
                            ok: false,
                            code: "COORDINATION_DENIED".into(),
                            message: "denied".into(),
                            payload: None,
                        }
                    };
                    ("prepare", response)
                }
                DaemonRequest::Coord {
                    op: ployz_api::CoordOp::Release { .. },
                } => (
                    "release",
                    DaemonResponse {
                        ok: true,
                        code: "OK".into(),
                        message: "released".into(),
                        payload: None,
                    },
                ),
                other => panic!("unexpected daemon request: {other:?}"),
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

    let result = state
        .reserve_machine_subnet(&MachineId("joiner".into()))
        .await;
    assert!(result.is_err());
    assert!(
        state
            .reservations
            .active_subnets(now_unix_secs())
            .await
            .is_empty()
    );

    server.await.expect("overlay server exit");
    let seen_requests = seen_requests.lock().await.clone();
    assert_eq!(
        seen_requests,
        vec![
            "prepare".to_string(),
            "prepare".to_string(),
            "prepare".to_string(),
            "release".to_string(),
            "release".to_string(),
            "release".to_string(),
        ]
    );

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_releases_reserved_subnet_when_remote_bootstrap_fails() {
    let _guard = test_ssh_env_lock().lock().await;
    let (state, _, _) = make_state(true).await;

    let ssh_dir = unique_temp_dir("ployz-fake-ssh-bootstrap-fail");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
    let _status_fail_guard =
        TestSshEnvGuard::set("PLOYZ_TEST_STATUS_FAIL_TARGETS", Some("join-target".into()));

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.ok, "{}", response.message);
    assert_eq!(response.code, "MACHINE_ADD_FAILED");
    assert!(
        state
            .reservations
            .active_subnets(now_unix_secs())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn machine_add_rejects_remote_subnet_mismatch_before_invite_consume() {
    let _guard = test_ssh_env_lock().lock().await;
    let (state, store, _) = make_state(true).await;

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-mismatch".into()),
        public_key: PublicKey([14; 32]),
        overlay_ip: "fd00::14".parse().map(OverlayIp).expect("valid overlay"),
        subnet: Some("10.210.99.0/24".parse().expect("valid subnet")),
        endpoints: vec!["203.0.113.14:51820".into()],
    }
    .encode()
    .expect("encode join response");

    let ssh_dir = unique_temp_dir("ployz-fake-ssh-mismatch");
    std::fs::create_dir_all(&ssh_dir).expect("create ssh dir");
    let fake_ssh = write_fake_ssh(&ssh_dir);
    let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
    let self_record_response = serde_json::to_string(&DaemonResponse {
        ok: true,
        code: "OK".into(),
        message: join_response.clone(),
        payload: Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload {
            encoded: join_response.clone(),
            record: JoinResponse::decode(&join_response)
                .expect("decode join response")
                .into_seed_machine_record(),
        })),
    })
    .expect("encode self-record response");
    let _join_guard = TestSshEnvGuard::set(
        "PLOYZ_TEST_SELF_RECORD_RESPONSE",
        Some(self_record_response.into()),
    );

    let response = state
        .handle_machine_add(&["join-target".into()], &MachineAddOptions::default())
        .await;
    assert!(!response.ok, "{}", response.message);
    assert_eq!(response.code, "MACHINE_ADD_FAILED");
    assert!(response.message.contains("founder reserved"));

    let invites = store.list_invites().await.expect("list invites");
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0].consumed_by, None);
    assert!(
        state
            .reservations
            .active_subnets(now_unix_secs())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn mesh_promote_restores_subnet_before_enable_finalization() {
    let (mut state, store, _) = make_state(true).await;
    let standby_response = state.handle_mesh_standby(true).await;
    assert!(standby_response.ok, "{}", standby_response.message);

    let promote_response = state
        .handle_mesh_promote("10.210.2.0/24".parse().expect("valid subnet"))
        .await;
    assert!(promote_response.ok, "{}", promote_response.message);

    let promoted = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(promoted.participation, Participation::Disabled);
    assert_eq!(
        promoted.subnet,
        Some("10.210.2.0/24".parse().expect("valid subnet"))
    );
    assert_eq!(promoted.status, MachineStatus::Up);
    assert_eq!(
        state.active.as_ref().expect("active mesh").config.subnet,
        Some("10.210.2.0/24".parse().expect("valid subnet"))
    );

    let enable_response = state
        .handle_mesh_set_participation(Participation::Enabled)
        .await;
    assert!(enable_response.ok, "{}", enable_response.message);

    let local = store
        .list_machines()
        .await
        .expect("list machines")
        .into_iter()
        .find(|machine| machine.id == state.identity.machine_id)
        .expect("local machine present");
    assert_eq!(local.participation, Participation::Enabled);
    assert_eq!(
        local.subnet,
        Some("10.210.2.0/24".parse().expect("valid subnet"))
    );
}

#[tokio::test]
async fn machine_enable_rolls_back_remote_promote_when_self_record_fails() {
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
        Participation::Disabled,
        now_unix_secs(),
        PublicKey([7; 32]),
    );
    peer.overlay_ip = "::1".parse().map(OverlayIp).expect("valid overlay");
    peer.subnet = None;
    peer.status = MachineStatus::Up;
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
            let (label, response) = match request {
                DaemonRequest::MeshPromote { .. } => (
                    "promote",
                    DaemonResponse {
                        ok: true,
                        code: "OK".into(),
                        message: "promoted".into(),
                        payload: None,
                    },
                ),
                DaemonRequest::MeshSelfRecord => (
                    "self_record",
                    DaemonResponse {
                        ok: false,
                        code: "BROKEN_SELF_RECORD".into(),
                        message: "self record unavailable".into(),
                        payload: None,
                    },
                ),
                DaemonRequest::MeshStandby { force } => {
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
                other => panic!("unexpected daemon request: {other:?}"),
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

    let response = state.handle_machine_enable("peer").await;
    assert!(!response.ok, "{}", response.message);
    assert_eq!(response.code, "SELF_RECORD_FAILED");
    assert!(
        state
            .reservations
            .active_subnets(now_unix_secs())
            .await
            .is_empty()
    );

    server.await.expect("overlay server exit");
    let seen_requests = seen_requests.lock().await.clone();
    assert_eq!(
        seen_requests,
        vec![
            "promote".to_string(),
            "self_record".to_string(),
            "standby".to_string()
        ]
    );

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn memory_mode_local_subnet_heal_updates_local_config_and_store() {
    let store = Arc::new(MemoryStore::new());
    store
        .upsert_self_machine(&test_machine_record(
            "founder",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert founder");
    store
        .upsert_self_machine(&test_machine_record(
            "peer",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([3; 32]),
        ))
        .await
        .expect("upsert peer");

    let mut state = make_state_with_store(
        Identity::generate(MachineId("peer".into()), [3; 32]),
        "10.210.1.0/24",
        store.clone(),
    )
    .await;
    state
        .active
        .as_mut()
        .expect("active mesh")
        .mesh
        .up()
        .await
        .expect("mesh up");

    state.heal_local_subnet_conflict_if_needed().await;

    let Some(pending) = state.pending_subnet_heal else {
        panic!("expected pending heal after first pass");
    };
    let initial_config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("load config after reservation");
    assert_eq!(
        initial_config.subnet,
        Some("10.210.1.0/24".parse().expect("valid"))
    );
    let reserved_peer = store
        .list_machines()
        .await
        .expect("list machines after reservation")
        .into_iter()
        .find(|machine| machine.id.0 == "peer")
        .expect("peer present after reservation");
    assert_eq!(reserved_peer.subnet, Some(pending.target_subnet));
    assert_eq!(reserved_peer.participation, Participation::Disabled);

    state.pending_subnet_heal = Some(crate::daemon::PendingSubnetHeal {
        planned_at: pending.planned_at.saturating_sub(20),
        ..pending
    });
    state.heal_local_subnet_conflict_if_needed().await;

    let healed_config = NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha"))
        .expect("load healed config");
    assert_eq!(
        healed_config.subnet,
        Some("10.210.0.0/24".parse().expect("valid"))
    );
    let machines = store.list_machines().await.expect("list machines");
    let peer = machines
        .into_iter()
        .find(|machine| machine.id.0 == "peer")
        .expect("peer present");
    assert_eq!(peer.subnet, Some("10.210.0.0/24".parse().expect("valid")));
    assert_eq!(
        state
            .active
            .as_ref()
            .map(|active| active.config.subnet)
            .expect("active config present"),
        Some("10.210.0.0/24".parse().expect("valid"))
    );

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn local_subnet_heal_skips_when_store_unhealthy() {
    let store = Arc::new(MemoryStore::new());
    store
        .upsert_self_machine(&test_machine_record(
            "founder",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert founder");
    store
        .upsert_self_machine(&test_machine_record(
            "peer",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([3; 32]),
        ))
        .await
        .expect("upsert peer");

    let mut state = make_state_with_store(
        Identity::generate(MachineId("peer".into()), [3; 32]),
        "10.210.1.0/24",
        store,
    )
    .await;
    state
        .active
        .as_mut()
        .expect("active mesh")
        .mesh
        .up()
        .await
        .expect("mesh up");

    let service = state
        .active
        .as_ref()
        .expect("active")
        .mesh
        .store
        .memory_service()
        .expect("expected memory store");
    service.set_healthy(false);

    state.heal_local_subnet_conflict_if_needed().await;

    let healed_config =
        NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha")).expect("load config");
    assert_eq!(
        healed_config.subnet,
        Some("10.210.1.0/24".parse().expect("valid"))
    );

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn local_subnet_heal_skips_when_mesh_not_running() {
    let store = Arc::new(MemoryStore::new());
    store
        .upsert_self_machine(&test_machine_record(
            "founder",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert founder");
    store
        .upsert_self_machine(&test_machine_record(
            "peer",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([3; 32]),
        ))
        .await
        .expect("upsert peer");

    let mut state = make_state_with_store(
        Identity::generate(MachineId("peer".into()), [3; 32]),
        "10.210.1.0/24",
        store,
    )
    .await;

    state.heal_local_subnet_conflict_if_needed().await;

    let healed_config =
        NetworkConfig::load(&NetworkConfig::path(&state.data_dir, "alpha")).expect("load config");
    assert_eq!(
        healed_config.subnet,
        Some("10.210.1.0/24".parse().expect("valid"))
    );
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
        Participation::Disabled,
        0,
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
        1,
    );
    state.active = Some(ActiveMesh {
        config,
        mesh,
        remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        peer_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
    });

    (state, store, network)
}

async fn make_state_with_store(
    identity: Identity,
    subnet: &str,
    store: Arc<MemoryStore>,
) -> DaemonState {
    let subnet: Ipv4Net = subnet.parse().expect("valid subnet");
    let data_dir = unique_temp_dir("ployz-machine-heal-state");
    let config = NetworkConfig::new(
        ployz_types::model::NetworkName("alpha".into()),
        &identity.public_key,
        DEFAULT_CLUSTER_CIDR,
        subnet,
    );
    config
        .save(&NetworkConfig::path(&data_dir, "alpha"))
        .expect("save config");

    let mesh = Mesh::new(
        WireguardDriver::memory_with(Arc::new(MemoryWireGuard::new())),
        StoreDriver::memory_with(store, Arc::new(MemoryService::new())),
        None,
        identity.machine_id.clone(),
        51820,
    );

    let mut state = DaemonState::new_for_tests(
        &data_dir,
        identity,
        DEFAULT_CLUSTER_CIDR.into(),
        24,
        4317,
        "127.0.0.1:0".into(),
        1,
    );
    state.active = Some(ActiveMesh {
        config,
        mesh,
        remote_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        peer_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
        dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
    });
    state
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
    participation: Participation,
    last_heartbeat: u64,
    public_key: PublicKey,
) -> MachineRecord {
    MachineRecord {
        id: MachineId(id.into()),
        public_key,
        overlay_ip: format!("fd00::{id_len:x}", id_len = id.len())
            .parse()
            .map(OverlayIp)
            .expect("valid overlay"),
        control_target: Some(id.into()),
        subnet: Some(subnet.parse().expect("valid subnet")),
        bridge_ip: None,
        endpoints: vec!["127.0.0.1:51820".into()],
        status: MachineStatus::Unknown,
        participation,
        last_heartbeat,
        created_at: 0,
        updated_at: 0,
        labels: std::collections::BTreeMap::new(),
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}

fn write_fake_ssh(dir: &PathBuf) -> PathBuf {
    let script = dir.join("ssh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprev=''\nfor arg in \"$@\"; do\n  target=\"$prev\"\n  command=\"$arg\"\n  prev=\"$arg\"\ndone\nif [ \"$command\" = 'set -eu; \"$HOME/.local/bin/ployz\" rpc-stdio' ]; then\n  req=$(cat)\n  case \"$req\" in\n    *'\"Coord\"'*)\n      case \"$req\" in\n        *'\"Prepare\"'*)\n          case \",$PLOYZ_TEST_COORD_PREPARE_DENY_TARGETS,\" in\n            *\",$target,\"*) printf '{\"ok\":false,\"code\":\"COORDINATION_DENIED\",\"message\":\"denied\",\"payload\":null}' ;;\n            *) printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"allow\",\"payload\":null}' ;;\n          esac\n          ;;\n        *'\"Release\"'*)\n          case \",$PLOYZ_TEST_COORD_RELEASE_DENY_TARGETS,\" in\n            *\",$target,\"*) printf '{\"ok\":false,\"code\":\"COORDINATION_DENIED\",\"message\":\"denied\",\"payload\":null}' ;;\n            *) printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"allow\",\"payload\":null}' ;;\n          esac\n          ;;\n        *)\n          printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"ack\",\"payload\":null}'\n          ;;\n      esac\n      ;;\n    *'\"MeshBootstrap\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"bootstrapped\",\"payload\":null}'\n      ;;\n    *'\"MeshJoin\"'*)\n      printf '{\"ok\":false,\"code\":\"UNSUPPORTED\",\"message\":\"unsupported\",\"payload\":null}'\n      ;;\n    *'\"MeshInit\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"init\",\"payload\":null}'\n      ;;\n    *'\"MeshDestroy\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"destroyed\",\"payload\":null}'\n      ;;\n    *'\"MeshDown\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"down\",\"payload\":null}'\n      ;;\n    *'\"MeshSelfRecord\"'*)\n      printf '%s' \"$PLOYZ_TEST_SELF_RECORD_RESPONSE\"\n      ;;\n    *'\"MeshReady\"'*)\n      printf '%s' \"$PLOYZ_TEST_READY_RESPONSE\"\n      ;;\n    *)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"ok\",\"payload\":null}'\n      ;;\n  esac\n  exit 0\nfi\ncase \"$command\" in\n  *'--version'*)\n    printf 'ployz test-version'\n    exit 0\n    ;;\n  *'status >/dev/null'*)\n    case \",$PLOYZ_TEST_STATUS_FAIL_TARGETS,\" in\n      *\",$target,\"*) exit 1 ;;\n      *) exit 0 ;;\n    esac\n    ;;\n  *'bash -s -- install'*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
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
