use crate::model::{NetworkId, NetworkName, OverlayIp, PublicKey, management_ip_from_key};
use ipnet::Ipv4Net;
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

/// Default cluster network for subnet allocation.
pub const DEFAULT_CLUSTER_CIDR: &str = "10.210.0.0/16";

/// Persistent network membership: which mesh this machine belongs to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub id: NetworkId,
    pub name: NetworkName,
    pub overlay_ip: OverlayIp,
    pub cluster_cidr: String,
    pub subnet: Ipv4Net,
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
            subnet,
        }
    }

    /// Path to network config dir: `<data_dir>/networks/<name>/`
    #[must_use]
    pub fn dir(data_dir: &Path, name: &str) -> PathBuf {
        ployz_sdk::paths::network_dir(data_dir, name)
    }

    /// Path to network config file: `<data_dir>/networks/<name>/network.json`
    #[must_use]
    pub fn path(data_dir: &Path, name: &str) -> PathBuf {
        ployz_sdk::paths::network_config_path(data_dir, name)
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
        std::fs::write(path, data).map_err(|source| NetworkConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Read the active network name from `<data_dir>/active_network`.
    #[must_use]
    pub fn read_active_network(data_dir: &Path) -> Option<String> {
        ployz_sdk::paths::read_active_network(data_dir)
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
    use crate::model::PublicKey;

    #[test]
    fn roundtrip_persists_network_id() {
        let root =
            std::env::temp_dir().join(format!("ployz-network-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        let subnet: Ipv4Net = "10.210.1.0/24".parse().unwrap();
        let cfg = NetworkConfig::new(
            NetworkName("alpha".into()),
            &PublicKey([7; 32]),
            DEFAULT_CLUSTER_CIDR,
            subnet,
        );
        let path = NetworkConfig::path(&root, "alpha");
        cfg.save(&path).expect("save config");

        let loaded = NetworkConfig::load(&path).expect("load config");
        assert_eq!(loaded.id, cfg.id);
        assert_eq!(loaded.name, cfg.name);

        let _ = std::fs::remove_dir_all(&root);
    }
}
