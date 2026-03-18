use super::heal::plan_local_subnet_heal;
use super::operations::{MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus};
use crate::daemon::ActiveMesh;
use crate::daemon::DaemonState;
use crate::daemon::ssh::{TestSshEnvGuard, TestSshProgramGuard, test_ssh_env_lock};
use ipnet::Ipv4Net;
use ployz_api::{DaemonPayload, DaemonResponse, MachineAddOptions, MeshSelfRecordPayload};
use ployz_orchestrator::mesh::driver::WireguardDriver;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_orchestrator::Mesh;
use ployz_state::node::identity::Identity;
use ployz_store_api::MachineStore;
use ployz_state::store::backends::memory::{MemoryService, MemoryStore};
use ployz_state::store::network::{DEFAULT_CLUSTER_CIDR, NetworkConfig};
use ployz_state::time::now_unix_secs;
use ployz_state::StoreDriver;
use ployz_types::model::{
    JoinResponse, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[tokio::test]
async fn allocate_machine_subnets_returns_unique_values() {
    let (state, store, _) = make_state(false).await;
    store
        .upsert_self_machine(&test_machine_record(
            "peer-1",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([2; 32]),
        ))
        .await
        .expect("upsert existing peer");

    let subnets = state
        .allocate_machine_subnets(3)
        .await
        .expect("allocate subnets");

    assert_eq!(subnets.len(), 3);
    assert_eq!(
        subnets
            .iter()
            .map(ToString::to_string)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert!(!subnets.contains(&"10.210.1.0/24".parse().expect("valid subnet")));
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
    let (mut state, store, network) = make_state(true).await;
    store
        .upsert_self_machine(&test_machine_record(
            "stale-peer",
            "10.210.1.0/24",
            Participation::Enabled,
            0,
            PublicKey([3; 32]),
        ))
        .await
        .expect("upsert stale peer");

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-1".into()),
        public_key: PublicKey([4; 32]),
        overlay_ip: "fd00::4".parse().map(OverlayIp).expect("valid overlay"),
        subnet: Some("10.210.2.0/24".parse().expect("valid subnet")),
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
        payload: Some(DaemonPayload::MeshSelfRecord(
            MeshSelfRecordPayload {
                encoded: join_response.clone(),
                record: JoinResponse::decode(&join_response)
                    .expect("decode join response")
                    .into_seed_machine_record(),
            },
        )),
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

    teardown_state(&mut state).await;
}

#[tokio::test]
async fn machine_add_accepts_running_joiner_before_full_sync() {
    let _guard = test_ssh_env_lock().lock().await;
    let (mut state, store, network) = make_state(true).await;

    let join_response = JoinResponse {
        machine_id: MachineId("joiner-2".into()),
        public_key: PublicKey([5; 32]),
        overlay_ip: "fd00::5".parse().map(OverlayIp).expect("valid overlay"),
        subnet: Some("10.210.1.0/24".parse().expect("valid subnet")),
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
        payload: Some(DaemonPayload::MeshSelfRecord(
            MeshSelfRecordPayload {
                encoded: join_response.clone(),
                record: JoinResponse::decode(&join_response)
                    .expect("decode join response")
                    .into_seed_machine_record(),
            },
        )),
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
        "10.210.1.0/24".parse().expect("valid")
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
        "10.210.0.0/24".parse().expect("valid")
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
        "10.210.0.0/24".parse().expect("valid")
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
        "10.210.1.0/24".parse().expect("valid")
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
        "10.210.1.0/24".parse().expect("valid")
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
    let identity = Identity::generate(MachineId("founder".into()), [1; 32]);
    let founder_subnet: Ipv4Net = "10.210.0.0/24".parse().expect("valid subnet");
    let config = NetworkConfig::new(
        ployz_types::model::NetworkName("alpha".into()),
        &identity.public_key,
        DEFAULT_CLUSTER_CIDR,
        founder_subnet,
    );

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
        &unique_temp_dir("ployz-machine-state"),
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
        "#!/bin/sh\nfor arg in \"$@\"; do\n  command=\"$arg\"\ndone\nif [ \"$command\" = 'set -eu; \"$HOME/.local/bin/ployz\" rpc-stdio' ]; then\n  req=$(cat)\n  case \"$req\" in\n    *'\"MeshJoin\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"joined\",\"payload\":null}'\n      ;;\n    *'\"MeshInit\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"init\",\"payload\":null}'\n      ;;\n    *'\"MeshDestroy\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"destroyed\",\"payload\":null}'\n      ;;\n    *'\"MeshDown\"'*)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"down\",\"payload\":null}'\n      ;;\n    *'\"MeshSelfRecord\"'*)\n      printf '%s' \"$PLOYZ_TEST_SELF_RECORD_RESPONSE\"\n      ;;\n    *'\"MeshReady\"'*)\n      printf '%s' \"$PLOYZ_TEST_READY_RESPONSE\"\n      ;;\n    *)\n      printf '{\"ok\":true,\"code\":\"OK\",\"message\":\"ok\",\"payload\":null}'\n      ;;\n  esac\n  exit 0\nfi\ncase \"$command\" in\n  *'--version'*)\n    printf 'ployz test-version'\n    exit 0\n    ;;\n  *'status >/dev/null'*)\n    exit 0\n    ;;\n  *'bash -s -- install'*)\n    cat >/dev/null\n    exit 0\n    ;;\n  *)\n    exit 0\n    ;;\nesac\n",
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
