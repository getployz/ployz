use ployz::{JoinResponse, MachineId, MachineRecord, OverlayIp, PublicKey};
use ployz::{MachineStore, Mesh, Phase, StoreDriver, SyncStatus, WireguardDriver};
use ployz::{MemoryService, MemoryStore, MemoryWireGuard};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;

fn test_record(id: &str, key_byte: u8) -> MachineRecord {
    MachineRecord {
        id: MachineId(id.into()),
        public_key: PublicKey([key_byte; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        subnet: None,
        bridge_ip: None,
        endpoints: vec![format!("10.0.0.{key_byte}:51820")],
        status: Default::default(),
        scheduling: Default::default(),
        last_heartbeat: 0,
        created_at: 0,
        updated_at: 0,
    }
}

fn make_mesh(wg: Arc<MemoryWireGuard>, svc: Arc<MemoryService>, store: Arc<MemoryStore>) -> Mesh {
    Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
        None,
        MachineId("test-machine".into()),
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

    store.upsert_machine(&test_record("m1", 1)).await.unwrap();

    let mut mesh = make_mesh(wg.clone(), svc.clone(), store);
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

    // No peers in store — single node.
    let mut mesh = make_mesh(wg, svc, store);
    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
}

#[tokio::test]
async fn detach_stops_tasks_leaves_infra() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let mut mesh = make_mesh(wg.clone(), svc.clone(), store);
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

    let mut mesh = make_mesh(wg, svc, store);
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

    let mut mesh = make_mesh(wg.clone(), svc, store);
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

    let mut mesh = make_mesh(wg.clone(), svc.clone(), store);
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

    store.upsert_machine(&test_record("m1", 1)).await.unwrap();

    // Store returns Disconnected forever.
    store.set_sync_status(SyncStatus::Disconnected);

    let mut mesh = Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
        None,
        MachineId("test-machine".into()),
        51820,
    )
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

    store.upsert_machine(&test_record("m1", 1)).await.unwrap();

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
        MachineId("test-machine".into()),
        51820,
    )
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5));

    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
}

// --- Two-node join ---

/// After a joiner joins the mesh, the founder's store must contain the joiner's
/// machine record so peer_sync can configure WireGuard. Without this, Corrosion
/// gossip can't flow (it runs over the overlay) and we have a chicken-and-egg.
#[tokio::test]
async fn founder_has_joiner_record_after_add() {
    // Founder node
    let founder_wg = Arc::new(MemoryWireGuard::new());
    let founder_svc = Arc::new(MemoryService::new());
    let founder_store = Arc::new(MemoryStore::new());

    let founder_record = test_record("founder", 1);
    founder_store.upsert_machine(&founder_record).await.unwrap();

    let mut founder_mesh = make_mesh(founder_wg.clone(), founder_svc, founder_store.clone());
    founder_mesh.up().await.unwrap();
    assert_eq!(founder_mesh.phase(), Phase::Running);

    // Simulate the JoinResponse flow: joiner builds a JoinResponse from its identity,
    // encodes it, founder decodes and upserts the record.
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
    let record = decoded.into_machine_record();

    // Founder seeds the joiner's record into its store (what machine add does)
    founder_store.upsert_machine(&record).await.unwrap();

    // Give peer_sync time to pick up the record.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The founder's store must contain the joiner's record for the overlay to form.
    let machines = founder_store.list_machines().await.unwrap();
    let has_joiner = machines.iter().any(|m| m.id.0 == "joiner");
    assert!(
        has_joiner,
        "founder's store must contain the joiner's record after machine add"
    );

    founder_mesh.destroy().await.unwrap();
}

// --- Store events ---

#[tokio::test]
async fn store_event_triggers_reconcile() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let mut mesh = make_mesh(wg.clone(), svc, store.clone());
    mesh.up().await.unwrap();

    let initial_count = wg.set_peers_count();

    // Add a peer via the store — should trigger event → reconcile.
    store.upsert_machine(&test_record("m2", 2)).await.unwrap();
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

    let peer = test_record("m2", 2);
    store.upsert_machine(&peer).await.unwrap();

    let mut mesh = make_mesh(wg.clone(), svc, store.clone());
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
