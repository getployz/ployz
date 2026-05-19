#![cfg(all(feature = "linux-wireguard", target_os = "linux"))]

use mvp_bus::IslandId;
use mvp_identity::NodeId;
use mvp_mesh::{
    LinuxWireGuardBackend, LinuxWireGuardConfig, WireGuardAppliedSnapshot, WireGuardBackend,
    WireGuardOverlayIp, WireGuardPrivateKey, WireGuardSnapshotPaths,
};

#[tokio::test]
async fn linux_wireguard_backend_applies_and_adopts_real_interface_when_enabled() {
    if std::env::var("MVP_LINUX_WG_SMOKE").as_deref() != Ok("1") {
        eprintln!("skipping real WireGuard smoke; set MVP_LINUX_WG_SMOKE=1 to run it");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let ifname = format!("mvpwg{}", std::process::id());
    let private_key = WireGuardPrivateKey::from_seed(&ifname);
    let config = LinuxWireGuardConfig::new(
        IslandId::new("prod"),
        &ifname,
        private_key,
        WireGuardSnapshotPaths::new(temp.path()),
    );
    let backend = LinuxWireGuardBackend::new(config).expect("linux wireguard backend");
    let snapshot = WireGuardAppliedSnapshot {
        island: "prod".to_string(),
        local_node_id: NodeId::new("node-a"),
        local_overlay_ip: WireGuardOverlayIp::new("fd00::a".parse().expect("overlay ip")),
        revision: 1,
        peers: Vec::new(),
    };

    backend
        .apply(snapshot.clone())
        .await
        .expect("apply snapshot");
    backend
        .apply(snapshot.clone())
        .await
        .expect("reapply same snapshot");

    assert_eq!(
        backend.last_applied().await.expect("last applied"),
        Some(snapshot)
    );

    let _ = std::process::Command::new("ip")
        .args(["link", "del", "dev", &ifname])
        .output();
}
