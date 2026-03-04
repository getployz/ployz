use ployz::{MachineId, MachineRecord, OverlayIp, PublicKey};
use ployz::{MachineStore, Mesh, WireguardDriver, Phase, StoreDriver, SyncStatus};
use ployz::{MemoryService, MemoryStore, MemoryWireGuard};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;

fn test_record(id: &str, key_byte: u8) -> MachineRecord {
    MachineRecord {
        id: MachineId(id.into()),
        public_key: PublicKey([key_byte; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        endpoints: vec![format!("10.0.0.{key_byte}:51820")],
    }
}

fn make_mesh(wg: Arc<MemoryWireGuard>, svc: Arc<MemoryService>, store: Arc<MemoryStore>) -> Mesh {
    Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
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
    )
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_millis(100));

    let err = mesh.up().await.unwrap_err();
    assert!(err.to_string().contains("bootstrap timeout"));
    assert_eq!(mesh.phase(), Phase::Stopped);
}

#[tokio::test]
async fn bootstrap_sync_completes() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store.upsert_machine(&test_record("m1", 1)).await.unwrap();

    store.set_sync_status(SyncStatus::Disconnected);

    let s = store.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        s.set_sync_status(SyncStatus::Syncing { gaps: 100 });
        tokio::time::sleep(Duration::from_millis(30)).await;
        s.set_sync_status(SyncStatus::Synced);
    });

    let mut mesh = Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
    )
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5));

    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
}

#[tokio::test]
async fn bootstrap_sync_waits_indefinitely() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store.upsert_machine(&test_record("m1", 1)).await.unwrap();

    // Start at Syncing (not Disconnected) — connection phase passes immediately.
    store.set_sync_status(SyncStatus::Syncing { gaps: 50 });

    let s = store.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        s.set_sync_status(SyncStatus::Synced);
    });

    // connection_timeout is very short (50ms), but store starts at Syncing
    // so connection phase passes immediately. Sync phase has no timeout.
    let mut mesh = Mesh::new(
        WireguardDriver::Memory(wg),
        StoreDriver::Memory {
            store,
            service: svc,
        },
    )
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_millis(50));

    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
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
