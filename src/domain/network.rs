use crate::domain::model::{management_ip_from_key, NetworkName, OverlayIp, PublicKey};
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

/// Persistent network membership: which mesh this machine belongs to.
#[derive(Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub name: NetworkName,
    pub overlay_ip: OverlayIp,
}

impl NetworkConfig {
    pub fn new(name: NetworkName, public_key: &PublicKey) -> Self {
        let overlay_ip = management_ip_from_key(public_key);
        Self { name, overlay_ip }
    }

    /// Path to network config dir: `<data_dir>/networks/<name>/`
    pub fn dir(data_dir: &Path, name: &str) -> PathBuf {
        data_dir.join("networks").join(name)
    }

    /// Path to network config file: `<data_dir>/networks/<name>/network.json`
    pub fn path(data_dir: &Path, name: &str) -> PathBuf {
        Self::dir(data_dir, name).join("network.json")
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

    pub fn delete(data_dir: &Path, name: &str) -> std::io::Result<()> {
        let dir = Self::dir(data_dir, name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// Scan for an existing network config in the data dir.
    /// Returns the first one found (one-at-a-time model).
    pub fn scan(data_dir: &Path) -> Option<Self> {
        let networks_dir = data_dir.join("networks");
        let entries = std::fs::read_dir(&networks_dir).ok()?;
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                let config_path = entry.path().join("network.json");
                if let Ok(config) = Self::load(&config_path) {
                    return Some(config);
                }
            }
        }
        None
    }
}
