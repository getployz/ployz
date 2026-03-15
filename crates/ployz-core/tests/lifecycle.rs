use ployz_core::mesh::tasks::{HeartbeatCommand, PeerSyncCommand};
use ployz_core::mesh::wireguard::MemoryWireGuard;
use ployz_core::model::{
    JoinResponse, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
};
use ployz_core::store::backends::memory::{MemoryService, MemoryStore};
use ployz_core::store::{MachineStore, SyncStatus};
use ployz_core::{Mesh, Phase, StoreDriver, WireguardDriver};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

fn test_record(id: &str, key_byte: u8) -> MachineRecord {
    MachineRecord {
        id: MachineId(id.into()),
        public_key: PublicKey([key_byte; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        subnet: None,
        bridge_ip: None,
        endpoints: vec![format!("10.0.0.{key_byte}:51820")],
        status: MachineStatus::Unknown,
        participation: Participation::Disabled,
        last_heartbeat: 0,
        created_at: 0,
        updated_at: 0,
        labels: std::collections::BTreeMap::new(),
    }
}

fn make_mesh(
    machine_id: &str,
    wg: Arc<MemoryWireGuard>,
    svc: Arc<MemoryService>,
    store: Arc<MemoryStore>,
) -> Mesh {
    Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
        None,
        MachineId(machine_id.into()),
        51820,
    )
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5))
}

// --- Success paths ---

#[tokio::test]
async fn startup_reaches_running_with_healthy_service() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store
        .upsert_self_machine(&test_record("m1", 1))
        .await
        .unwrap();

    let mut mesh = make_mesh("m1", wg.clone(), svc.clone(), store);
    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
    assert!(wg.is_up());

    mesh.destroy().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Stopped);
}

#[tokio::test]
async fn startup_reaches_running_single_node() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store
        .upsert_self_machine(&test_record("m1", 1))
        .await
        .unwrap();

    // No remote peers in store — single node.
    let mut mesh = make_mesh("m1", wg, svc, store);
    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);

    let ready = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let ready = mesh.ready_status().await;
            if ready.ready {
                return ready;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("single-node founder should become ready within the timeout");
    assert!(ready.ready, "single-node founder should report ready");
    assert!(
        ready.sync_connected,
        "single-node founder should not wait for remote sync"
    );

    let heartbeat_tx = mesh
        .heartbeat_sender()
        .expect("heartbeat coordinator should be running");
    for _ in 0..3 {
        let (done_tx, done_rx) = oneshot::channel();
        heartbeat_tx
            .send(HeartbeatCommand::TickNow { done: done_tx })
            .await
            .expect("manual heartbeat tick should send");
        done_rx
            .await
            .expect("manual heartbeat tick should acknowledge");
    }

    let self_record = mesh
        .authoritative_self_record()
        .await
        .expect("self record should exist");
    assert_eq!(self_record.participation, Participation::Enabled);
}

#[tokio::test]
async fn joiner_seed_peer_requires_sync_for_ready() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let founder_record = test_record("founder", 1);
    let joiner_record = test_record("joiner", 2);
    store.upsert_self_machine(&joiner_record).await.unwrap();
    store.set_sync_status(SyncStatus::Disconnected);

    let mut mesh = Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store: store.clone(),
            service: svc,
        },
        None,
        joiner_record.id.clone(),
        51820,
    )
    .with_seed_records(vec![founder_record])
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5));
    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);

    let ready = mesh.ready_status().await;
    assert!(
        !ready.sync_connected,
        "joiner with only a bootstrap seed peer should not report sync connected"
    );
    assert!(
        !ready.ready,
        "joiner with only a bootstrap seed peer should not report ready"
    );
}

#[tokio::test]
async fn joiner_retains_founder_peer_across_peer_sync_handoff() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let founder_record = test_record("founder", 1);
    let joiner_record = test_record("joiner", 2);
    store.upsert_self_machine(&joiner_record).await.unwrap();

    let mut mesh = Mesh::new(
        WireguardDriver::Memory(wg.clone()),
        StoreDriver::Memory {
            store: store.clone(),
            service: svc,
        },
        None,
        joiner_record.id.clone(),
        51820,
    )
    .with_seed_records(vec![founder_record.clone(), joiner_record.clone()])
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5));
    mesh.up().await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        wg.current_peers()
            .iter()
            .any(|peer| peer.id == founder_record.id),
        "bootstrap founder peer must remain configured before store convergence"
    );

    store.upsert_self_machine(&founder_record).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        wg.current_peers()
            .iter()
            .any(|peer| peer.id == founder_record.id),
        "founder peer must remain configured after store convergence"
    );
}

#[tokio::test]
async fn detach_stops_tasks_leaves_infra() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store
        .upsert_self_machine(&test_record("m1", 1))
        .await
        .unwrap();

    let mut mesh = make_mesh("m1", wg.clone(), svc.clone(), store);
    mesh.up().await.unwrap();

    mesh.detach().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Stopped);
    // WG is still up after detach — infra stays.
    assert!(wg.is_up());
    // Service was not stopped by detach.
    assert!(svc.is_started());
}

// --- Failure paths ---

#[tokio::test]
async fn component_failure_returns_to_stopped() {
    let wg = Arc::new(MemoryWireGuard::new());
    wg.set_fail_up(true);
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let mut mesh = make_mesh("m1", wg, svc, store);
    let err = mesh.up().await.unwrap_err();
    assert!(err.to_string().contains("injected failure"));
    assert_eq!(mesh.phase(), Phase::Stopped);
}

#[tokio::test]
async fn service_failure_tears_down_wg() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    svc.set_fail_start(true);
    let store = Arc::new(MemoryStore::new());

    let mut mesh = make_mesh("m1", wg.clone(), svc, store);
    let err = mesh.up().await.unwrap_err();
    assert!(err.to_string().contains("injected failure"));
    assert_eq!(mesh.phase(), Phase::Stopped);
    // WG should have been torn down after service failure.
    assert!(!wg.is_up());
}

#[tokio::test]
async fn destroy_continues_on_errors_returns_first() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store
        .upsert_self_machine(&test_record("m1", 1))
        .await
        .unwrap();

    let mut mesh = make_mesh("m1", wg.clone(), svc.clone(), store);
    mesh.up().await.unwrap();

    // Make both service stop and wg down fail.
    svc.set_fail_stop(true);
    wg.set_fail_down(true);

    let err = mesh.destroy().await.unwrap_err();
    // First error encountered was service stop.
    assert!(err.to_string().contains("service stop"));
    // But we still reached Stopped — teardown continued.
    assert_eq!(mesh.phase(), Phase::Stopped);
}

// --- Bootstrap ---

#[tokio::test]
async fn bootstrap_connection_timeout() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());
    let founder_record = test_record("founder", 1);
    let joiner_record = test_record("joiner", 2);

    store.upsert_self_machine(&joiner_record).await.unwrap();
    store.upsert_self_machine(&founder_record).await.unwrap();

    // Store returns Disconnected forever.
    store.set_sync_status(SyncStatus::Disconnected);

    let mut mesh = Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
        None,
        joiner_record.id.clone(),
        51820,
    )
    .with_seed_records(vec![founder_record])
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_millis(100));

    let err = mesh.up().await.unwrap_err();
    assert!(err.to_string().contains("bootstrap timeout"));
    assert_eq!(mesh.phase(), Phase::Stopped);
}

/// Bootstrap gate proceeds once gossip sees a peer (any non-Disconnected status).
/// We do NOT wait for gaps == 0 — see bootstrap_gate doc comment.
#[tokio::test]
async fn bootstrap_proceeds_on_membership() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());
    let founder_record = test_record("founder", 1);
    let joiner_record = test_record("joiner", 2);

    store.upsert_self_machine(&joiner_record).await.unwrap();
    store.upsert_self_machine(&founder_record).await.unwrap();

    store.set_sync_status(SyncStatus::Disconnected);

    let s = store.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Syncing (with gaps) is enough — gate doesn't wait for Synced.
        s.set_sync_status(SyncStatus::Syncing { gaps: 100 });
    });

    let mut mesh = Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
        None,
        joiner_record.id.clone(),
        51820,
    )
    .with_seed_records(vec![founder_record])
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5));

    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
}

// --- Two-node join ---

/// Founder-side join bootstrap must be able to configure a remote peer without
/// durably publishing that remote machine row into the store.
#[tokio::test]
async fn founder_can_configure_joiner_from_transient_peer() {
    // Founder node
    let founder_wg = Arc::new(MemoryWireGuard::new());
    let founder_svc = Arc::new(MemoryService::new());
    let founder_store = Arc::new(MemoryStore::new());

    let founder_record = test_record("founder", 1);
    founder_store
        .upsert_self_machine(&founder_record)
        .await
        .unwrap();

    let mut founder_mesh = make_mesh(
        "founder",
        founder_wg.clone(),
        founder_svc,
        founder_store.clone(),
    );
    founder_mesh.up().await.unwrap();
    assert_eq!(founder_mesh.phase(), Phase::Running);

    // Simulate the JoinResponse flow: joiner builds a JoinResponse from its identity
    // and founder installs it as a transient peer only.
    let joiner_record = test_record("joiner", 2);
    let join_resp = JoinResponse {
        machine_id: joiner_record.id.clone(),
        public_key: joiner_record.public_key.clone(),
        overlay_ip: joiner_record.overlay_ip,
        subnet: joiner_record.subnet,
        endpoints: joiner_record.endpoints.clone(),
    };

    // Encode → decode roundtrip (simulates SSH transport)
    let encoded = join_resp.encode().unwrap();
    let decoded = JoinResponse::decode(&encoded).unwrap();
    let record = decoded.into_seed_machine_record();

    founder_mesh
        .peer_sync_sender()
        .expect("peer sync sender")
        .send(PeerSyncCommand::UpsertTransient(record))
        .await
        .unwrap();

    // Give peer_sync time to pick up the record.
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        founder_wg
            .current_peers()
            .iter()
            .any(|peer| peer.id.0 == "joiner"),
        "transient joiner peer must be configured for the overlay to form"
    );

    let machines = founder_store.list_machines().await.unwrap();
    assert!(
        !machines.iter().any(|m| m.id.0 == "joiner"),
        "founder must not durably publish the joiner row"
    );

    founder_mesh.destroy().await.unwrap();
}

// --- Store events ---

#[tokio::test]
async fn store_event_triggers_reconcile() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store
        .upsert_self_machine(&test_record("m1", 1))
        .await
        .unwrap();

    let mut mesh = make_mesh("m1", wg.clone(), svc, store.clone());
    mesh.up().await.unwrap();

    let initial_count = wg.set_peers_count();

    // Add a peer via the store — should trigger event → reconcile.
    store
        .upsert_self_machine(&test_record("m2", 2))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        wg.set_peers_count() > initial_count,
        "set_peers should have been called after store event"
    );

    mesh.destroy().await.unwrap();
}

#[tokio::test]
async fn remove_event_drops_wireguard_peer() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store
        .upsert_self_machine(&test_record("m1", 1))
        .await
        .unwrap();

    let peer = test_record("m2", 2);
    store.upsert_self_machine(&peer).await.unwrap();

    let mut mesh = make_mesh("m1", wg.clone(), svc, store.clone());
    mesh.up().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        wg.current_peers()
            .iter()
            .any(|candidate| candidate.id == peer.id),
        "peer must be configured before removal"
    );

    store.delete_machine(&peer.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        !wg.current_peers()
            .iter()
            .any(|candidate| candidate.id == peer.id),
        "peer must be removed from wireguard after store delete"
    );

    mesh.destroy().await.unwrap();
}
