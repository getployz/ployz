use ipnet::Ipv4Net;
use ployz_types::model::{
    NetworkId, NetworkLifecycle, NetworkName, OverlayIp, PublicKey, RegionRole,
    StorageParticipation, StorageReplicaPolicy, management_ip_from_key,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, NetworkConfigError>;

#[derive(Debug, Error)]
pub enum NetworkConfigError {
    #[error("reading network config from {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing network config JSON")]
    Parse(#[source] serde_json::Error),
    #[error("creating directory {path}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serializing network config")]
    Serialize(#[source] serde_json::Error),
    #[error("writing network config to {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg_attr(not(test), allow(dead_code))]
pub const DEFAULT_CLUSTER_CIDR: &str = "10.210.0.0/16";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub id: NetworkId,
    pub name: NetworkName,
    pub overlay_ip: OverlayIp,
    pub cluster_cidr: String,
    #[serde(default)]
    pub lifecycle: NetworkLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<Ipv4Net>,
    pub storage: bool,
    pub storage_participation: StorageParticipation,
    pub region_role: RegionRole,
    pub storage_replicas: StorageReplicaPolicy,
}

impl NetworkConfig {
    #[must_use]
    pub fn new(
        name: NetworkName,
        public_key: &PublicKey,
        cluster_cidr: &str,
        subnet: Ipv4Net,
    ) -> Self {
        let overlay_ip = management_ip_from_key(public_key);
        Self {
            id: NetworkId::random(),
            name,
            overlay_ip,
            cluster_cidr: cluster_cidr.to_string(),
            lifecycle: NetworkLifecycle::Stopped,
            subnet: Some(subnet),
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            region_role: RegionRole::HomeData,
            storage_replicas: StorageReplicaPolicy::Single,
        }
    }

    #[must_use]
    pub fn dir(data_dir: &Path, name: &str) -> PathBuf {
        ployz_config::network_dir(data_dir, name)
    }

    #[must_use]
    pub fn path(data_dir: &Path, name: &str) -> PathBuf {
        ployz_config::network_config_path(data_dir, name)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path).map_err(|source| NetworkConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&data).map_err(NetworkConfigError::Parse)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| {
                NetworkConfigError::CreateDirectory {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }
        let data = serde_json::to_string_pretty(self).map_err(NetworkConfigError::Serialize)?;
        let tmp_path = path.with_extension(format!(
            "{}.tmp",
            path.extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("json")
        ));
        std::fs::write(&tmp_path, data).map_err(|source| NetworkConfigError::Write {
            path: tmp_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, path).map_err(|source| NetworkConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
    }
    pub fn delete(data_dir: &Path, name: &str) -> std::io::Result<()> {
        let dir = Self::dir(data_dir, name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_persists_network_intent() {
        let root =
            std::env::temp_dir().join(format!("ployz-network-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let subnet: Ipv4Net = "10.210.1.0/24".parse().expect("valid subnet");
        let mut cfg = NetworkConfig::new(
            NetworkName("alpha".into()),
            &PublicKey([7; 32]),
            DEFAULT_CLUSTER_CIDR,
            subnet,
        );
        cfg.region_role = RegionRole::Compute;
        cfg.storage_replicas = StorageReplicaPolicy::R3;
        let path = NetworkConfig::path(&root, "alpha");
        cfg.save(&path).expect("save config");

        let loaded = NetworkConfig::load(&path).expect("load config");
        assert_eq!(loaded.id, cfg.id);
        assert_eq!(loaded.name, cfg.name);
        assert_eq!(loaded.region_role, RegionRole::Compute);
        assert_eq!(loaded.storage_replicas, StorageReplicaPolicy::R3);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_rejects_missing_storage_replica_intent() {
        let root = std::env::temp_dir().join(format!(
            "ployz-network-missing-replicas-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let path = NetworkConfig::path(&root, "alpha");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("create dir");
        std::fs::write(
            &path,
            r#"{
  "id": "net-test",
  "name": "alpha",
  "overlay_ip": "fd00::1",
  "cluster_cidr": "10.210.0.0/16",
  "lifecycle": "stopped",
  "subnet": "10.210.1.0/24",
  "storage": true,
  "storage_participation": { "kind": "authority", "authority_id": "auth-default" },
  "region_role": "home_data"
}"#,
        )
        .expect("write config");

        NetworkConfig::load(&path).expect_err("missing replica intent should fail");

        let _ = std::fs::remove_dir_all(&root);
    }
}
