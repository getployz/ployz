use ployz::{ConvergenceConfig, MachineStore, Mesh, Phase};
use ployz::{MachineId, MachineRecord, NetworkId, NetworkName, OverlayIp, PublicKey};
use ployz::{MemoryService, MemoryStore, MemorySyncProbe, MemoryWireGuard};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

fn test_network_id() -> NetworkId {
    NetworkId("test-net".into())
}

fn test_record(id: &str, key_byte: u8) -> MachineRecord {
    MachineRecord {
        id: MachineId(id.into()),
        network_id: test_network_id(),
        network: NetworkName("test".into()),
        public_key: PublicKey([key_byte; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        endpoints: vec![format!("10.0.0.{key_byte}:51820")],
    }
}

fn make_mesh(
    wg: Arc<MemoryWireGuard>,
    svc: Arc<MemoryService>,
    store: Arc<MemoryStore>,
) -> Mesh<MemoryWireGuard, MemoryService, MemoryStore, MemoryWireGuard, MemorySyncProbe> {
    Mesh::new(test_network_id(), wg.clone(), svc, store, Some(wg), None)
        .with_convergence_config(ConvergenceConfig {
            probe_interval: Duration::from_millis(50),
            handshake_timeout: Duration::from_millis(200),
            rotation_timeout: Duration::from_millis(200),
        })
        .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5), 2)
}

// --- Success paths ---

#[tokio::test]
async fn startup_reaches_running_with_healthy_service() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    store
        .upsert_machine(&test_network_id(), &test_record("m1", 1))
        .await
        .unwrap();

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
async fn detach_stops_convergence_leaves_infra() {
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
async fn bootstrap_timeout_returns_typed_error() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    // Peer key exists in handshake map but with no handshake — network gate blocks.
    wg.set_handshake(PublicKey([1; 32]), None);

    let mut mesh: Mesh<
        MemoryWireGuard,
        MemoryService,
        MemoryStore,
        MemoryWireGuard,
        MemorySyncProbe,
    > = Mesh::new(test_network_id(), wg.clone(), svc, store, Some(wg), None).with_bootstrap_timing(
        Duration::from_millis(10),
        Duration::from_millis(100),
        2,
    );

    let err = mesh.up().await.unwrap_err();
    assert!(err.to_string().contains("bootstrap timeout"));
    assert_eq!(mesh.phase(), Phase::Stopped);
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

// --- Two-stage bootstrap ---

#[tokio::test]
async fn two_stage_bootstrap_with_sync_probe() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());
    let sync_probe = Arc::new(MemorySyncProbe::new());

    // Start with sync incomplete.
    sync_probe.set_synced(false);

    // Flip sync to complete after a short delay.
    let sync_clone = sync_probe.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        sync_clone.set_synced(true);
    });

    let mut mesh: Mesh<
        MemoryWireGuard,
        MemoryService,
        MemoryStore,
        MemoryWireGuard,
        MemorySyncProbe,
    > = Mesh::new(test_network_id(), wg, svc, store, None, Some(sync_probe)).with_bootstrap_timing(
        Duration::from_millis(10),
        Duration::from_secs(5),
        2,
    );

    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
}

#[tokio::test]
async fn sync_gate_timeout() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());
    let sync_probe = Arc::new(MemorySyncProbe::new());

    // Sync never completes.
    sync_probe.set_synced(false);

    let mut mesh: Mesh<
        MemoryWireGuard,
        MemoryService,
        MemoryStore,
        MemoryWireGuard,
        MemorySyncProbe,
    > = Mesh::new(test_network_id(), wg, svc, store, None, Some(sync_probe)).with_bootstrap_timing(
        Duration::from_millis(10),
        Duration::from_millis(100),
        2,
    );

    let err = mesh.up().await.unwrap_err();
    assert!(err.to_string().contains("bootstrap timeout"));
    assert_eq!(mesh.phase(), Phase::Stopped);
}

#[tokio::test]
async fn single_node_network_gate_passes() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    // No peers, no sync prober — both gates skip.
    let mut mesh: Mesh<
        MemoryWireGuard,
        MemoryService,
        MemoryStore,
        MemoryWireGuard,
        MemorySyncProbe,
    > = Mesh::new(test_network_id(), wg.clone(), svc, store, Some(wg), None).with_bootstrap_timing(
        Duration::from_millis(10),
        Duration::from_secs(5),
        2,
    );

    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
}

// --- Convergence behavior ---

#[tokio::test]
async fn store_event_triggers_reconcile() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let mut mesh = make_mesh(wg.clone(), svc, store.clone());
    mesh.up().await.unwrap();

    let initial_count = wg.set_peers_count();

    // Add a peer via the store — should trigger event → reconcile.
    store
        .upsert_machine(&test_network_id(), &test_record("m2", 2))
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
async fn endpoint_rotation_on_stale_handshake() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let key = PublicKey([0xAA; 32]);
    let record = MachineRecord {
        id: MachineId("m-multi".into()),
        network_id: test_network_id(),
        network: NetworkName("test".into()),
        public_key: key.clone(),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        endpoints: vec!["a:1".into(), "b:2".into()],
    };
    store
        .upsert_machine(&test_network_id(), &record)
        .await
        .unwrap();

    // Give a stale handshake so rotation fires.
    let stale = Instant::now() - Duration::from_secs(60);
    wg.set_handshake(key, Some(stale));

    let mut mesh = make_mesh(wg.clone(), svc, store);
    mesh.up().await.unwrap();

    // Wait for a couple probe intervals to allow rotation.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The probe should have caused at least one set_peers call beyond initial reconcile.
    assert!(
        wg.set_peers_count() >= 2,
        "expected rotation to trigger set_peers"
    );

    mesh.destroy().await.unwrap();
}

#[tokio::test]
async fn fresh_handshake_keeps_endpoint_sticky() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let key = PublicKey([0xBB; 32]);
    let record = MachineRecord {
        id: MachineId("m-sticky".into()),
        network_id: test_network_id(),
        network: NetworkName("test".into()),
        public_key: key.clone(),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        endpoints: vec!["a:1".into(), "b:2".into()],
    };
    store
        .upsert_machine(&test_network_id(), &record)
        .await
        .unwrap();

    // Fresh handshake — should NOT rotate.
    wg.set_handshake(key.clone(), Some(Instant::now()));

    let mut mesh = make_mesh(wg.clone(), svc, store);
    mesh.up().await.unwrap();

    // Continuously refresh the handshake so the probe always sees it as fresh.
    let wg2 = wg.clone();
    let key2 = key.clone();
    let refresher = tokio::spawn(async move {
        for _ in 0..20 {
            wg2.set_handshake(key2.clone(), Some(Instant::now()));
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    });

    // Let several probe intervals pass.
    tokio::time::sleep(Duration::from_millis(400)).await;
    refresher.await.unwrap();

    // The endpoint should stay at "a:1" — no rotation because handshake stays fresh.
    let peers = wg.current_peers();
    if let Some(p) = peers.iter().find(|p| p.id.0 == "m-sticky") {
        assert_eq!(
            p.endpoints,
            vec!["a:1".to_string()],
            "endpoint should be sticky"
        );
    }

    mesh.destroy().await.unwrap();
}
