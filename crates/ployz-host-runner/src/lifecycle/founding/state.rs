//! Durable founding state and single-process exclusion.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ployz_core::founding::FoundingArrival;
use ployz_core::ids::{ClusterName, SubjectTokenError};

const CLUSTER_ID_FILE: &str = "cluster-id";
const COMPLETE_FILE: &str = "founding-complete";
pub(crate) const JOINED_FILE: &str = "join-complete";
const LOCK_FILE: &str = ".init.lock";
const MILESTONE_DIRECTORY: &str = "founding";

/// Existing directory in which machine identity and founding evidence live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundingStateDirectory(PathBuf);

impl FoundingStateDirectory {
    /// Creates a root-owned private state directory on a clean host.
    pub fn initialize(path: impl Into<PathBuf>) -> Result<Self, FoundingStateError> {
        let path = path.into();
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|error| FoundingStateError::CreateDirectory {
                path: path.clone(),
                message: error.to_string(),
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| FoundingStateError::ProtectDirectory {
                    path: path.clone(),
                    message: error.to_string(),
                },
            )?;
        }
        Self::open(path)
    }

    /// Initializes the production machine state path.
    pub fn initialize_host_default() -> Result<Self, FoundingStateError> {
        Self::initialize("/var/lib/ployz")
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, FoundingStateError> {
        let path = path.into();
        let metadata =
            std::fs::metadata(&path).map_err(|error| FoundingStateError::OpenDirectory {
                path: path.clone(),
                message: error.to_string(),
            })?;
        if !metadata.is_dir() {
            return Err(FoundingStateError::NotDirectory { path });
        }
        Ok(Self(path))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn try_lock(&self) -> Result<FoundingInitLock, FoundingStateError> {
        let path = self.0.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| FoundingStateError::OpenLock {
                path: path.clone(),
                message: error.to_string(),
            })?;
        match file.try_lock() {
            Ok(()) => Ok(FoundingInitLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(FoundingStateError::InitInProgress),
            Err(std::fs::TryLockError::Error(error)) => Err(FoundingStateError::LockFailed {
                path,
                message: error.to_string(),
            }),
        }
    }

    pub fn observe_arrival(&self) -> Result<FoundingArrival, FoundingStateError> {
        let cluster_id_path = self.0.join(CLUSTER_ID_FILE);
        let complete = self.0.join(COMPLETE_FILE).exists();
        let joined = self.0.join(JOINED_FILE).exists();
        let cluster_id = match std::fs::read_to_string(&cluster_id_path) {
            Ok(value) => Some(
                ClusterName::try_new(value.trim_end_matches(['\r', '\n'])).map_err(|source| {
                    FoundingStateError::InvalidClusterId {
                        path: cluster_id_path.clone(),
                        source,
                    }
                })?,
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(FoundingStateError::ReadClusterId {
                    path: cluster_id_path,
                    message: error.to_string(),
                });
            }
        };
        let Some(persisted_cluster_id) = cluster_id else {
            if complete || joined {
                return Err(FoundingStateError::MarkerWithoutClusterId);
            }
            return Ok(FoundingArrival::Clean);
        };
        if joined {
            return Ok(FoundingArrival::Joined {
                persisted_cluster_id,
            });
        }
        if complete {
            return Ok(FoundingArrival::Complete {
                persisted_cluster_id,
            });
        }
        Ok(FoundingArrival::Partial {
            persisted_cluster_id,
        })
    }

    /// Persists the resume anchor without replacing any existing identity.
    pub fn persist_cluster_id_exclusive(
        &self,
        cluster_id: &ClusterName,
    ) -> Result<(), FoundingStateError> {
        self.persist_cluster_id_exclusive_with(cluster_id, || Ok(()))
    }

    fn persist_cluster_id_exclusive_with(
        &self,
        cluster_id: &ClusterName,
        before_publish: impl FnOnce() -> io::Result<()>,
    ) -> Result<(), FoundingStateError> {
        let path = self.0.join(CLUSTER_ID_FILE);
        let mut staged = crate::StagedFile::create(
            &self.0,
            CLUSTER_ID_FILE,
            "cluster-id",
            crate::FileMode::Secret0600,
        )
        .map_err(|error| FoundingStateError::StageClusterId {
            path: path.clone(),
            message: format!("{error:?}"),
        })?;
        writeln!(staged.file(), "{}", cluster_id.as_str()).map_err(|error| {
            FoundingStateError::WriteClusterId {
                path: path.clone(),
                message: error.to_string(),
            }
        })?;
        staged
            .file()
            .sync_all()
            .map_err(|error| FoundingStateError::WriteClusterId {
                path: path.clone(),
                message: error.to_string(),
            })?;
        before_publish().map_err(|error| FoundingStateError::PublishClusterId {
            path: path.clone(),
            message: error.to_string(),
        })?;
        std::fs::hard_link(staged.path(), &path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                return FoundingStateError::ClusterIdAlreadyExists { path: path.clone() };
            }
            FoundingStateError::PublishClusterId {
                path: path.clone(),
                message: error.to_string(),
            }
        })?;
        sync_directory(&self.0).map_err(|error| FoundingStateError::SyncDirectory {
            path: self.0.clone(),
            message: error.to_string(),
        })
    }

    pub fn milestone_complete(
        &self,
        milestone: FoundingMilestone,
    ) -> Result<bool, FoundingStateError> {
        let path = self.0.join(MILESTONE_DIRECTORY).join(milestone.file_name());
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(FoundingStateError::InvalidMilestone { path }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(FoundingStateError::ReadMilestone {
                path,
                message: error.to_string(),
            }),
        }
    }

    pub fn record_milestone(&self, milestone: FoundingMilestone) -> Result<(), FoundingStateError> {
        let directory = self.0.join(MILESTONE_DIRECTORY);
        std::fs::create_dir_all(&directory).map_err(|error| {
            FoundingStateError::CreateMilestoneDirectory {
                path: directory.clone(),
                message: error.to_string(),
            }
        })?;
        crate::write_durable_file(
            &directory,
            milestone.file_name(),
            crate::FileMode::Plain,
            b"complete\n",
        )
        .map_err(|message| FoundingStateError::WriteMilestone {
            path: directory.join(milestone.file_name()),
            message: message.to_string(),
        })?;
        if milestone == FoundingMilestone::Complete {
            crate::write_durable_file(
                &self.0,
                COMPLETE_FILE,
                crate::FileMode::Plain,
                b"complete\n",
            )
            .map_err(|message| FoundingStateError::WriteMilestone {
                path: self.0.join(COMPLETE_FILE),
                message: message.to_string(),
            })?;
        }
        Ok(())
    }
}

/// Held for the complete observe/classify/execute founding attempt.
#[derive(Debug)]
pub struct FoundingInitLock {
    file: File,
}

impl Drop for FoundingInitLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundingMilestone {
    Artifacts,
    Docker,
    MachineMaterial,
    DoorMaterial,
    Storage,
    Configuration,
    FoundingRequest,
    DockerConfigured,
    UnitsInstalledReadyEnabled,
    KeeperStarted,
    CorrosionStarted,
    BootstrapApiStarted,
    RowsSubmitted,
    DriverPeerConverged,
    DriverEnrolledReported,
    EndpointNetworkReady,
    GatewayStarted,
    DnsStarted,
    DnsReady,
    BootstrapReady,
    BootstrapCredentialRemoved,
    OrdinaryApiStarted,
    OrdinaryReady,
    DriverReadyReported,
    Complete,
}

impl FoundingMilestone {
    const fn file_name(self) -> &'static str {
        // These names are durable schema keys. New milestones append numeric
        // identities even when their execution point precedes older steps.
        match self {
            Self::Artifacts => "01-artifacts",
            Self::Docker => "02-docker",
            Self::MachineMaterial => "03-machine-material",
            Self::FoundingRequest => "04-founding-request",
            Self::DoorMaterial => "05-door-material",
            Self::Storage => "07-storage",
            Self::Configuration => "08-configuration",
            Self::DockerConfigured => "09-docker-configured",
            Self::UnitsInstalledReadyEnabled => "10-units-installed-ready-enabled",
            Self::KeeperStarted => "11-keeper-started",
            Self::CorrosionStarted => "12-corrosion-started",
            Self::BootstrapApiStarted => "13-bootstrap-api-started",
            Self::RowsSubmitted => "14-rows-submitted",
            Self::DriverPeerConverged => "15-driver-peer-converged",
            Self::DriverEnrolledReported => "16-driver-enrolled-reported",
            Self::EndpointNetworkReady => "17-endpoint-network-ready",
            Self::GatewayStarted => "26-gateway-started",
            Self::DnsStarted => "24-dns-started",
            Self::DnsReady => "25-dns-ready",
            Self::BootstrapReady => "18-bootstrap-ready",
            Self::BootstrapCredentialRemoved => "19-bootstrap-credential-removed",
            Self::OrdinaryApiStarted => "20-ordinary-api-started",
            Self::OrdinaryReady => "21-ordinary-ready",
            Self::DriverReadyReported => "22-driver-ready-reported",
            Self::Complete => "23-complete",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FoundingStateError {
    #[error("failed to create founding state directory {}: {message}", path.display())]
    CreateDirectory { path: PathBuf, message: String },
    #[error("failed to protect founding state directory {}: {message}", path.display())]
    ProtectDirectory { path: PathBuf, message: String },
    #[error("failed to open founding state directory {}: {message}", path.display())]
    OpenDirectory { path: PathBuf, message: String },
    #[error("founding state path {} is not a directory", path.display())]
    NotDirectory { path: PathBuf },
    #[error("failed to open founding lock {}: {message}", path.display())]
    OpenLock { path: PathBuf, message: String },
    #[error("another ployz init is already running")]
    InitInProgress,
    #[error("failed to lock {}: {message}", path.display())]
    LockFailed { path: PathBuf, message: String },
    #[error("failed to read cluster id {}: {message}", path.display())]
    ReadClusterId { path: PathBuf, message: String },
    #[error("cluster id file {} is invalid: {source}", path.display())]
    InvalidClusterId {
        path: PathBuf,
        source: SubjectTokenError,
    },
    #[error("founding or join completion marker exists without a cluster id")]
    MarkerWithoutClusterId,
    #[error("cluster id already exists at {}", path.display())]
    ClusterIdAlreadyExists { path: PathBuf },
    #[error("failed to stage cluster id for {}: {message}", path.display())]
    StageClusterId { path: PathBuf, message: String },
    #[error("failed to write cluster id {}: {message}", path.display())]
    WriteClusterId { path: PathBuf, message: String },
    #[error("failed to publish cluster id {}: {message}", path.display())]
    PublishClusterId { path: PathBuf, message: String },
    #[error("failed to sync directory {}: {message}", path.display())]
    SyncDirectory { path: PathBuf, message: String },
    #[error("failed to read founding milestone {}: {message}", path.display())]
    ReadMilestone { path: PathBuf, message: String },
    #[error("founding milestone {} is not a file", path.display())]
    InvalidMilestone { path: PathBuf },
    #[error("failed to create founding milestone directory {}: {message}", path.display())]
    CreateMilestoneDirectory { path: PathBuf, message: String },
    #[error("failed to write founding milestone {}: {message}", path.display())]
    WriteMilestone { path: PathBuf, message: String },
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_excludes_a_concurrent_init() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let held = state.try_lock().expect("first lock");

        assert!(matches!(
            state.try_lock(),
            Err(FoundingStateError::InitInProgress)
        ));

        drop(held);
        state.try_lock().expect("released lock can be acquired");
    }

    #[test]
    fn cluster_id_create_never_replaces_existing_identity() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let first = ClusterName::try_new("alpha").expect("cluster name");
        let second = ClusterName::try_new("beta").expect("cluster name");

        state
            .persist_cluster_id_exclusive(&first)
            .expect("first identity persists");
        assert!(matches!(
            state.persist_cluster_id_exclusive(&second),
            Err(FoundingStateError::ClusterIdAlreadyExists { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(directory.path().join(CLUSTER_ID_FILE))
                .expect("identity reads")
                .trim(),
            first.as_str()
        );
    }

    #[test]
    fn failed_publication_never_exposes_a_partial_cluster_id() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let cluster_id = ClusterName::try_new("alpha").expect("cluster name");

        assert!(matches!(
            state.persist_cluster_id_exclusive_with(&cluster_id, || {
                Err(io::Error::other("injected before publish"))
            }),
            Err(FoundingStateError::PublishClusterId { .. })
        ));
        assert!(!directory.path().join(CLUSTER_ID_FILE).exists());
        assert_eq!(
            state.observe_arrival().expect("arrival remains readable"),
            FoundingArrival::Clean
        );
    }
}
