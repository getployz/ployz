use std::collections::BTreeSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, VolumeError>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeBackendKind {
    Zfs,
    Btrfs,
    Docker,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeCapability {
    Ensure,
    Remove,
    Mount,
    Inspect,
    Snapshot,
    Clone,
    Send,
    Receive,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeCapabilities {
    supported: BTreeSet<VolumeCapability>,
}

impl VolumeCapabilities {
    #[must_use]
    pub fn new(supported: impl IntoIterator<Item = VolumeCapability>) -> Self {
        Self {
            supported: supported.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn zfs_like() -> Self {
        Self::new([
            VolumeCapability::Ensure,
            VolumeCapability::Remove,
            VolumeCapability::Mount,
            VolumeCapability::Inspect,
            VolumeCapability::Snapshot,
        ])
    }

    #[must_use]
    pub fn docker_local() -> Self {
        Self::new([
            VolumeCapability::Ensure,
            VolumeCapability::Remove,
            VolumeCapability::Mount,
            VolumeCapability::Inspect,
        ])
    }

    #[must_use]
    pub fn contains(&self, capability: VolumeCapability) -> bool {
        self.supported.contains(&capability)
    }

    pub fn require(&self, backend: &str, capability: VolumeCapability) -> Result<()> {
        if self.contains(capability) {
            Ok(())
        } else {
            Err(VolumeError::UnsupportedCapability {
                backend: backend.to_string(),
                capability,
            })
        }
    }

    #[must_use]
    pub fn iter(&self) -> impl Iterator<Item = VolumeCapability> + '_ {
        self.supported.iter().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeBackendInfo {
    pub id: String,
    pub kind: VolumeBackendKind,
    pub capabilities: VolumeCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VolumeIdentity {
    pub namespace: String,
    pub name: String,
}

impl VolumeIdentity {
    #[must_use]
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnsureVolumeRequest {
    pub identity: VolumeIdentity,
    pub mountpoint: Option<PathBuf>,
    pub quota: Option<String>,
    pub mode: Option<String>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMount {
    pub identity: VolumeIdentity,
    pub source: VolumeMountSource,
}

impl VolumeMount {
    #[must_use]
    pub fn host_path(identity: VolumeIdentity, path: PathBuf) -> Self {
        Self {
            identity,
            source: VolumeMountSource::HostPath { path },
        }
    }

    #[must_use]
    pub fn docker_volume(identity: VolumeIdentity, name: impl Into<String>) -> Self {
        Self {
            identity,
            source: VolumeMountSource::DockerVolume { name: name.into() },
        }
    }

    #[must_use]
    pub fn mountpoint(&self) -> Option<&PathBuf> {
        match &self.source {
            VolumeMountSource::HostPath { path } => Some(path),
            VolumeMountSource::DockerVolume { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeMountSource {
    HostPath { path: PathBuf },
    DockerVolume { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRequest {
    pub identity: VolumeIdentity,
    pub snapshot: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSnapshot {
    pub identity: VolumeIdentity,
    pub snapshot: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneVolumeRequest {
    pub source: VolumeIdentity,
    pub target: VolumeIdentity,
    pub snapshot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInspection {
    pub identity: VolumeIdentity,
    pub mountpoint: Option<PathBuf>,
    pub used_bytes: Option<u64>,
    pub snapshots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VolumeError {
    #[error("volume backend '{backend}' does not support {capability:?}")]
    UnsupportedCapability {
        backend: String,
        capability: VolumeCapability,
    },
    #[error("invalid volume request field '{field}': {message}")]
    InvalidRequest { field: String, message: String },
    #[error("volume backend '{backend}' failed during {operation}: {message}")]
    Backend {
        backend: String,
        operation: &'static str,
        message: String,
    },
}

#[async_trait]
pub trait VolumeBackend: Send + Sync {
    fn info(&self) -> VolumeBackendInfo;

    async fn ensure(&self, request: EnsureVolumeRequest) -> Result<VolumeMount>;

    async fn remove(&self, identity: VolumeIdentity) -> Result<()>;

    async fn mount(&self, identity: VolumeIdentity) -> Result<VolumeMount>;

    async fn inspect(&self, identity: VolumeIdentity) -> Result<Option<VolumeInspection>>;

    async fn snapshot(&self, request: SnapshotRequest) -> Result<VolumeSnapshot> {
        let _ = request;
        let info = self.info();
        Err(VolumeError::UnsupportedCapability {
            backend: info.id,
            capability: VolumeCapability::Snapshot,
        })
    }

    async fn clone_volume(&self, request: CloneVolumeRequest) -> Result<VolumeMount> {
        let _ = request;
        let info = self.info();
        Err(VolumeError::UnsupportedCapability {
            backend: info.id,
            capability: VolumeCapability::Clone,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_local_capabilities_do_not_claim_snapshot_semantics() {
        let capabilities = VolumeCapabilities::docker_local();

        assert!(capabilities.contains(VolumeCapability::Ensure));
        assert!(capabilities.contains(VolumeCapability::Mount));
        assert!(!capabilities.contains(VolumeCapability::Snapshot));
        assert!(!capabilities.contains(VolumeCapability::Send));
    }

    #[test]
    fn unsupported_capability_is_structured() {
        let capabilities = VolumeCapabilities::docker_local();

        let error = capabilities
            .require("docker-local", VolumeCapability::Clone)
            .expect_err("docker local clone should be unsupported");

        assert_eq!(
            error,
            VolumeError::UnsupportedCapability {
                backend: "docker-local".to_string(),
                capability: VolumeCapability::Clone,
            }
        );
    }
}
