use super::render::render_doctor_report;
use super::report::build_doctor_payload;
use crate::daemon::{ActiveMesh, DaemonState};
use crate::mesh_state::network::NetworkConfig;
use ployz_api::DaemonPayload;
use ployz_model::{
    MachineId, MachineLifecycle, MachineMembership, MachineTopology, NetworkLifecycle, OverlayIp,
    PublicKey,
};
use ployz_orchestrator::Mesh;
use ployz_orchestrator::mesh::DevicePeer;
use ployz_orchestrator::mesh::driver::WireguardDriver;
use ployz_orchestrator::mesh::wireguard::MemoryWireGuard;
use ployz_runtime_api::Identity;
use ployz_store_api::{MachineMembershipStore, PeerRttObservation, StoreDriver};
use ployz_store_memory::{MemoryService, MemoryStore, StoreDriverMemoryExt as _};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

#[tokio::test]
async fn doctor_reports_missing_required_peer_handshake() {
    let (state, store, network) = make_state().await;
    let peer_key = PublicKey([2; 32]);
    let stale_key = PublicKey([3; 32]);

    store
        .upsert_self_machine(&test_machine_record(
            "peer",
            MachineLifecycle::Active,
            peer_key.clone(),
        ))
        .await
        .expect("upsert peer");
    store
        .upsert_self_machine(&test_machine_record(
            "stale-peer",
            MachineLifecycle::Active,
            stale_key.clone(),
        ))
        .await
        .expect("upsert stale peer");

    network.set_device_peers(vec![DevicePeer {
        public_key: stale_key,
        endpoint: Some(String::from("127.0.0.1:51820")),
        last_handshake: Some(Instant::now() - Duration::from_secs(31)),
    }]);

    let response = state.handle_doctor().await;
    assert!(response.is_ok(), "{}", response.message());
    let Some(DaemonPayload::Doctor(payload)) = response.payload() else {
        panic!("expected doctor payload");
    };
    assert_eq!(payload.overall.lifecycle, "blocked");
    assert!(payload.peers.iter().any(|peer| {
        peer.machine_id == "peer"
            && peer.blocking
            && peer.role == "blocking"
            && peer.store_lifecycle == "active"
            && peer.wg_state == "absent"
            && peer.cause_code == "no-direct-peer"
    }));
    assert!(
        payload
            .peers
            .iter()
            .any(|peer| { peer.machine_id == "stale-peer" && peer.wg_state == "stale" })
    );
    assert!(response.message().contains("lifecycle: blocked"));
    assert!(response.message().contains("blocking peers:"));
    assert!(response.message().lines().any(|line| {
        line.contains("peer")
            && line.contains("store=active")
            && line.contains("wg=absent")
            && line.contains("rtt=none")
            && line.contains("cause=no direct peer is configured")
    }));
    assert!(response.message().contains("all peers:"));
    assert!(response.message().lines().any(|line| {
        line.contains("stale-peer") && line.contains("store=active") && line.contains("wg=stale")
    }));
    assert!(!response.message().contains("probe="));
}

#[tokio::test]
async fn doctor_reports_healthy_when_wireguard_handshake_is_fresh() {
    let (state, store, network) = make_state().await;
    let peer_key = PublicKey([2; 32]);

    store
        .upsert_self_machine(&test_machine_record(
            "peer",
            MachineLifecycle::Active,
            peer_key.clone(),
        ))
        .await
        .expect("upsert peer");

    network.set_device_peers(vec![DevicePeer {
        public_key: peer_key,
        endpoint: Some(String::from("127.0.0.1:51820")),
        last_handshake: Some(Instant::now()),
    }]);

    let response = state.handle_doctor().await;
    assert!(response.is_ok(), "{}", response.message());
    let Some(DaemonPayload::Doctor(payload)) = response.payload() else {
        panic!("expected doctor payload");
    };
    assert_eq!(payload.overall.lifecycle, "healthy");
    assert!(payload.peers.iter().any(|peer| {
        peer.machine_id == "peer"
            && !peer.blocking
            && peer.wg_state == "fresh"
            && peer.cause_code == "fresh-wireguard-handshake"
    }));
    assert!(response.message().contains("lifecycle: healthy"));
    assert!(!response.message().contains("blocking peers:"));
    assert!(response.message().contains("all peers:"));
    assert!(response.message().lines().any(|line| {
        line.contains("peer")
            && line.contains("store=active")
            && line.contains("wg=fresh")
            && line.contains("rtt=none")
    }));
    assert!(!response.message().contains("probe="));
}

#[tokio::test]
async fn doctor_projects_rtt_and_wireguard_state() {
    let local_record =
        test_machine_record("joiner5", MachineLifecycle::Standby, PublicKey([1; 32]));
    let mut peer_record = test_machine_record("peer", MachineLifecycle::Active, PublicKey([2; 32]));
    peer_record.overlay_ip = OverlayIp("fd00::2".parse().expect("valid overlay"));
    let machines = vec![local_record.clone(), peer_record.clone()];
    let peer_addr = SocketAddr::new(IpAddr::V6(peer_record.overlay_ip.0), 51001);
    let rtts = vec![PeerRttObservation {
        addr: peer_addr,
        rtts_ms: vec![120, 140, 160],
    }];
    let payload = build_doctor_payload(
        &test_active_mesh(),
        machines.as_slice(),
        &local_record,
        &[],
        rtts.as_slice(),
        &local_record.endpoints,
        true,
    );
    let report = render_doctor_report(&payload);

    assert_eq!(payload.overall.lifecycle, "blocked");
    assert!(payload.peers.iter().any(|peer| {
        peer.machine_id == "peer"
            && peer.rtt_median_ms == Some(140.0)
            && peer.cause_code == "no-direct-peer"
    }));
    assert!(report.contains("lifecycle: blocked"));
    assert!(report.contains("wg=absent"));
    assert!(report.contains("rtt=140ms±16.3ms"));
    assert!(!report.contains("probe="));
}

async fn make_state() -> (DaemonState, Arc<MemoryStore>, Arc<MemoryWireGuard>) {
    let identity = Identity::generate(MachineId::new(String::from("joiner5")), [1; 32]);
    let config = NetworkConfig::new(
        ployz_model::NetworkName(String::from("alpha")),
        &identity.public_key,
        "10.210.0.0/16",
        "10.210.3.0/24".parse().expect("valid subnet"),
    );
    let store = Arc::new(MemoryStore::new());
    let service = Arc::new(MemoryService::new());
    let network = Arc::new(MemoryWireGuard::new());

    store
        .upsert_self_machine(&test_machine_record(
            "joiner5",
            MachineLifecycle::Standby,
            identity.public_key.clone(),
        ))
        .await
        .expect("upsert self");

    let mesh = Mesh::new(
        WireguardDriver::memory_with(network.clone()),
        StoreDriver::memory_with(store.clone(), service),
        None,
        identity.machine_id.clone(),
        51820,
    );

    let mut state = DaemonState::new_for_tests(
        &unique_temp_dir("ployz-doctor-state"),
        identity,
        String::from("10.210.0.0/16"),
        24,
        4319,
        String::from("127.0.0.1:0"),
        None,
        1,
    );
    let retained_subnet = crate::daemon::RetainedSubnet::from_running_config(config.subnet);
    state.active = Some(ActiveMesh {
        config,
        retained_subnet,
        mesh,
        runtime: ployz_node_runtime::RuntimeComponents::noop(),
        image_receiver_bind_addr: None,

        certificate_renewal: None,
        bootstrap_peer_seed: None,
    });

    (state, store, network)
}

fn test_machine_record(
    id: &str,
    lifecycle: MachineLifecycle,
    public_key: PublicKey,
) -> MachineMembership {
    MachineMembership {
        id: MachineId::new(String::from(id)),
        public_key,
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        topology: MachineTopology::local(),
        region_role: ployz_model::RegionRole::HomeData,
        subnet: Some("10.210.0.0/24".parse().expect("valid subnet")),
        bridge_ip: None,
        endpoints: vec![String::from("127.0.0.1:51820")],
        lifecycle,
        storage_role: ployz_model::StorageParticipation::default_authority().into(),
        created_at: 0,
        updated_at: 0,
        labels: std::collections::BTreeMap::new(),
    }
}

fn test_active_mesh() -> ActiveMesh {
    let identity = Identity::generate(MachineId::new(String::from("joiner5")), [1; 32]);
    let mut config = NetworkConfig::new(
        ployz_model::NetworkName(String::from("alpha")),
        &identity.public_key,
        "10.210.0.0/16",
        "10.210.3.0/24".parse().expect("valid subnet"),
    );
    config.lifecycle = NetworkLifecycle::Running;
    let store = Arc::new(MemoryStore::new());
    let service = Arc::new(MemoryService::new());
    let network = Arc::new(MemoryWireGuard::new());
    let mesh = Mesh::new(
        WireguardDriver::memory_with(network),
        StoreDriver::memory_with(store, service),
        None,
        identity.machine_id,
        51820,
    );
    let retained_subnet = crate::daemon::RetainedSubnet::from_running_config(config.subnet);

    ActiveMesh {
        config,
        retained_subnet,
        mesh,
        runtime: ployz_node_runtime::RuntimeComponents::noop(),
        image_receiver_bind_addr: None,

        certificate_renewal: None,
        bootstrap_peer_seed: None,
    }
}

fn unique_temp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
}
