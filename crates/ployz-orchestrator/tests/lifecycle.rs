use ployz_orchestrator::mesh::WireGuardDevice;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_orchestrator::{Mesh, Phase, WireguardDriver};
use ployz_store_api::StoreDriver;
use ployz_store_api::memory::{MemoryService, MemoryStore};
use ployz_store_api::{MachineRegistry, SyncStatus};
use ployz_types::model::{
    MachineId, MachineLifecycle, MachineMembership, MachineTopology, OverlayIp, PublicKey,
    StorageParticipation,
};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::Duration;

fn test_record(id: &str, key_byte: u8) -> MachineMembership {
    MachineMembership {
        id: MachineId(id.into()),
        public_key: PublicKey([key_byte; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        topology: MachineTopology::local(),
        subnet: None,
        bridge_ip: None,
        endpoints: vec![format!("10.0.0.{key_byte}:51820")],
        lifecycle: MachineLifecycle::Standby,
        storage: true,
        storage_participation: StorageParticipation::default_authority(),
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
        WireguardDriver::memory_with(wg),
        StoreDriver::memory_with(store, svc),
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

    let self_record = mesh
        .authoritative_self_record()
        .await
        .expect("self record should exist");
    assert_eq!(self_record.lifecycle, MachineLifecycle::Standby);
}

#[tokio::test]
async fn startup_preserves_stored_self_state_when_seed_rebuilds_runtime_fields() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let mut stored = test_record("m1", 1);
    stored.lifecycle = MachineLifecycle::Active;
    stored.created_at = 11;
    stored.updated_at = 12;
    stored.labels = std::collections::BTreeMap::from([("role".into(), "api".into())]);
    stored.bridge_ip = Some(OverlayIp("fd00::10".parse().expect("valid bridge ip")));
    stored.endpoints = vec!["old.example:51820".into()];
    store.upsert_self_machine(&stored).await.unwrap();

    let mut seed = test_record("m1", 9);
    seed.overlay_ip = OverlayIp("fd00::9".parse().expect("valid overlay ip"));
    seed.topology =
        MachineTopology::new("seed-region", Some("seed-a")).expect("valid seed topology");
    seed.subnet = Some("10.210.9.0/24".parse().expect("valid subnet"));
    seed.endpoints = vec!["new.example:51820".into()];

    let mut mesh = Mesh::new(
        WireguardDriver::memory_with(wg),
        StoreDriver::memory_with(store.clone(), svc),
        None,
        stored.id.clone(),
        51820,
    )
    .with_seed_records(vec![seed.clone()])
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5));

    mesh.up().await.unwrap();

    let self_record = mesh
        .authoritative_self_record()
        .await
        .expect("self record should exist");
    assert_eq!(self_record.lifecycle, MachineLifecycle::Active);
    assert_eq!(self_record.created_at, stored.created_at);
    assert!(self_record.updated_at >= stored.updated_at);
    assert_eq!(self_record.labels, stored.labels);
    assert_eq!(self_record.public_key, seed.public_key);
    assert_eq!(self_record.overlay_ip, seed.overlay_ip);
    assert_eq!(self_record.topology, seed.topology);
    assert_eq!(self_record.subnet, seed.subnet);
    assert_eq!(self_record.endpoints, seed.endpoints);
    assert_eq!(self_record.bridge_ip, None);

    let stored_after = store
        .list_machines()
        .await
        .unwrap()
        .into_iter()
        .find(|machine| machine.id == stored.id)
        .expect("stored self record");
    assert_eq!(stored_after.lifecycle, MachineLifecycle::Active);
    assert_eq!(stored_after.labels, stored.labels);
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
        WireguardDriver::memory_with(wg),
        StoreDriver::memory_with(store.clone(), svc),
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
        WireguardDriver::memory_with(wg.clone()),
        StoreDriver::memory_with(store.clone(), svc),
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
            .any(|peer| peer.id() == &founder_record.id),
        "bootstrap founder peer must remain configured before store convergence"
    );

    store.upsert_self_machine(&founder_record).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        wg.current_peers()
            .iter()
            .any(|peer| peer.id() == &founder_record.id),
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
        WireguardDriver::memory_with(wg),
        StoreDriver::memory_with(store, svc),
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
        WireguardDriver::memory_with(wg),
        StoreDriver::memory_with(store, svc),
        None,
        joiner_record.id.clone(),
        51820,
    )
    .with_seed_records(vec![founder_record])
    .with_bootstrap_timing(Duration::from_millis(10), Duration::from_secs(5));

    mesh.up().await.unwrap();
    assert_eq!(mesh.phase(), Phase::Running);
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
            .any(|candidate| candidate.id() == &peer.id),
        "peer must be configured before removal"
    );

    store.delete_machine(&peer.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        !wg.current_peers()
            .iter()
            .any(|candidate| candidate.id() == &peer.id),
        "peer must be removed from wireguard after store delete"
    );

    mesh.destroy().await.unwrap();
}

#[tokio::test]
async fn manual_endpoint_maintenance_tick_rotates_down_peer_endpoint() {
    let wg = Arc::new(MemoryWireGuard::new());
    let svc = Arc::new(MemoryService::new());
    let store = Arc::new(MemoryStore::new());

    let local = test_record("m1", 1);
    let mut peer = test_record("m2", 2);
    peer.endpoints = vec!["198.51.100.10:51820".into(), "198.51.100.11:51820".into()];

    store.upsert_self_machine(&local).await.unwrap();
    store.upsert_self_machine(&peer).await.unwrap();
    wg.set_device_peers(vec![ployz_orchestrator::mesh::DevicePeer {
        public_key: peer.public_key.clone(),
        endpoint: Some("198.51.100.10:51820".into()),
        last_handshake: None,
    }]);

    let mut mesh = make_mesh("m1", wg.clone(), svc, store);
    mesh.up().await.unwrap();

    assert!(
        mesh.run_endpoint_maintenance_once(true).await,
        "endpoint maintainer tick should succeed"
    );

    let device_peers = wg.read_peers().await.unwrap();
    let peer = device_peers.first().expect("one device peer present");
    assert_eq!(peer.endpoint.as_deref(), Some("198.51.100.11:51820"));

    mesh.destroy().await.unwrap();
}
