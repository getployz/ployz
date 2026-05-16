use std::path::{Path, PathBuf};

pub const NODE_RPC_LISTENER_COMPONENT: &str = "node_rpc_listener";
pub const CERT_RENEWAL_WORKER_COMPONENT: &str = "cert_renewal_worker";
pub const BOOTSTRAP_PEER_SEED_COMPONENT: &str = "bootstrap_peer_seed";

pub const NATS_NODE_RPC_HEALTH_FILE: &str = "nats-node-rpc-health.json";
pub const NATS_CERT_RENEWAL_HEALTH_FILE: &str = "nats-cert-renewal-health.json";
pub const BOOTSTRAP_PEER_SEED_HEALTH_FILE: &str = "bootstrap-peer-seed-health.json";

pub type RuntimeComponentHealth = ployz_supervision::ComponentHealth;

#[must_use]
pub fn node_rpc_health_path(network_dir: &Path) -> PathBuf {
    network_dir.join(NATS_NODE_RPC_HEALTH_FILE)
}

#[must_use]
pub fn cert_renewal_health_path(network_dir: &Path) -> PathBuf {
    network_dir.join(NATS_CERT_RENEWAL_HEALTH_FILE)
}

#[must_use]
pub fn bootstrap_peer_seed_health_path(network_dir: &Path) -> PathBuf {
    network_dir.join(BOOTSTRAP_PEER_SEED_HEALTH_FILE)
}

pub async fn load_node_rpc_health(
    path: impl AsRef<Path>,
) -> std::io::Result<RuntimeComponentHealth> {
    ployz_supervision::load_component_health(path).await
}

pub async fn load_cert_renewal_health(
    path: impl AsRef<Path>,
) -> std::io::Result<RuntimeComponentHealth> {
    ployz_supervision::load_component_health(path).await
}

pub fn load_bootstrap_peer_seed_health(
    network_dir: &Path,
) -> Result<Option<RuntimeComponentHealth>, String> {
    let path = bootstrap_peer_seed_health_path(network_dir);
    if !path.exists() {
        return Ok(None);
    }
    ployz_supervision::load_component_health_sync(&path)
        .map(Some)
        .map_err(|error| {
            format!(
                "load bootstrap peer seed health '{}': {error}",
                path.display()
            )
        })
}

pub fn write_bootstrap_peer_seed_health(
    network_dir: &Path,
    health: &RuntimeComponentHealth,
) -> Result<(), String> {
    let path = bootstrap_peer_seed_health_path(network_dir);
    ployz_supervision::write_component_health_atomic_sync(&path, health).map_err(|error| {
        format!(
            "write bootstrap peer seed health '{}': {error}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn runtime_health_paths_use_stable_file_names() {
        let network_dir = Path::new("/tmp/ployz-network");

        assert_eq!(
            node_rpc_health_path(network_dir),
            PathBuf::from("/tmp/ployz-network/nats-node-rpc-health.json")
        );
        assert_eq!(
            cert_renewal_health_path(network_dir),
            PathBuf::from("/tmp/ployz-network/nats-cert-renewal-health.json")
        );
        assert_eq!(
            bootstrap_peer_seed_health_path(network_dir),
            PathBuf::from("/tmp/ployz-network/bootstrap-peer-seed-health.json")
        );
    }

    #[tokio::test]
    async fn cert_renewal_health_roundtrips_through_runtime_loader() {
        let path = temp_path("cert-renewal-health-roundtrip").join("health.json");
        let first = RuntimeComponentHealth::stale(1_777_646_000, None, "first");
        let health = RuntimeComponentHealth::stale(1_777_646_100, Some(&first), "fetch failed");

        ployz_supervision::write_component_health(&path, &health)
            .await
            .expect("write health");

        let loaded = load_cert_renewal_health(path).await.expect("load health");
        assert_eq!(loaded, health);
    }

    #[test]
    fn bootstrap_peer_seed_health_roundtrips_through_runtime_loader() {
        let network_dir = temp_path("bootstrap-peer-seed-health");
        let first = RuntimeComponentHealth::stale(100, None, "first");
        let health = RuntimeComponentHealth::stale(123, Some(&first), "watch failed");

        write_bootstrap_peer_seed_health(&network_dir, &health).expect("write health");
        let loaded = load_bootstrap_peer_seed_health(&network_dir)
            .expect("load health")
            .expect("health");

        assert_eq!(loaded, health);
        let _ = std::fs::remove_dir_all(&network_dir);
    }

    fn temp_path(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }
}
