//! Machine-local storage preparation contracts and durable evidence.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};

use crate::deploy::{DatasetName, ZfsPoolName};
use crate::ids::OperationId;

pub const PLOYZ_OWNED_ZFS_POOL: &str = "ployz";
pub const PLOYZ_OWNED_ZFS_BACKING_FILE: &str = "/var/lib/ployz/zfs/ployz.img";
pub const PROVISIONED_VOLUME_MOUNTPOINT: &str = "/var/lib/ployz/volumes";
pub const STORAGE_OPERATION_EVIDENCE_DIRECTORY: &str = "storage-operations";
const MACHINE_STORAGE_PREPARE_BUDGET_SECONDS: u64 = 30 * 60;
const MACHINE_STORAGE_PREPARE_TERMINATION_GRACE_SECONDS: u64 = 30;
pub const MACHINE_STORAGE_PREPARE_BUDGET: Duration =
    Duration::from_secs(MACHINE_STORAGE_PREPARE_BUDGET_SECONDS);
pub const MACHINE_STORAGE_PREPARE_TERMINATION_GRACE: Duration =
    Duration::from_secs(MACHINE_STORAGE_PREPARE_TERMINATION_GRACE_SECONDS);
pub const MACHINE_STORAGE_PREPARE_RPC_TIMEOUT: Duration = Duration::from_secs(
    MACHINE_STORAGE_PREPARE_BUDGET_SECONDS + MACHINE_STORAGE_PREPARE_TERMINATION_GRACE_SECONDS,
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparedStorageOrigin {
    OwnedImage { backing_file: PathBuf },
    Adopted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ZfsDatasetRoot(String);

impl ZfsDatasetRoot {
    #[must_use]
    pub fn for_pool(pool: &ZfsPoolName) -> Self {
        Self(format!("{}/ployz/volumes", pool.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn contains(&self, dataset: &DatasetName) -> bool {
        dataset
            .as_str()
            .strip_prefix(self.as_str())
            .and_then(|suffix| suffix.strip_prefix('/'))
            .is_some_and(|leaf| !leaf.is_empty() && !leaf.contains('/'))
    }
}

impl TryFrom<String> for ZfsDatasetRoot {
    type Error = PreparedStorageStateError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let Some(pool) = value.strip_suffix("/ployz/volumes") else {
            return Err(PreparedStorageStateError::InvalidDatasetRoot { value });
        };
        let pool = ZfsPoolName::try_new(pool).map_err(|_| {
            PreparedStorageStateError::InvalidDatasetRoot {
                value: value.clone(),
            }
        })?;
        let expected = Self::for_pool(&pool);
        if expected.0 != value {
            return Err(PreparedStorageStateError::InvalidDatasetRoot { value });
        }
        Ok(expected)
    }
}

impl From<ZfsDatasetRoot> for String {
    fn from(value: ZfsDatasetRoot) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedStorageState {
    pool: ZfsPoolName,
    origin: PreparedStorageOrigin,
    dataset_root: ZfsDatasetRoot,
}

impl PreparedStorageState {
    pub fn try_new(
        pool: ZfsPoolName,
        origin: PreparedStorageOrigin,
        dataset_root: ZfsDatasetRoot,
    ) -> Result<Self, PreparedStorageStateError> {
        if dataset_root != ZfsDatasetRoot::for_pool(&pool) {
            return Err(PreparedStorageStateError::PoolDatasetMismatch { pool, dataset_root });
        }
        if let PreparedStorageOrigin::OwnedImage { backing_file } = &origin
            && (pool.as_str() != PLOYZ_OWNED_ZFS_POOL
                || backing_file != Path::new(PLOYZ_OWNED_ZFS_BACKING_FILE))
        {
            return Err(PreparedStorageStateError::InvalidOwnedImage {
                pool,
                backing_file: backing_file.clone(),
            });
        }
        Ok(Self {
            pool,
            origin,
            dataset_root,
        })
    }

    #[must_use]
    pub const fn pool(&self) -> &ZfsPoolName {
        &self.pool
    }

    #[must_use]
    pub const fn origin(&self) -> &PreparedStorageOrigin {
        &self.origin
    }

    #[must_use]
    pub const fn dataset_root(&self) -> &ZfsDatasetRoot {
        &self.dataset_root
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedStorageStateWire {
    pool: ZfsPoolName,
    origin: PreparedStorageOrigin,
    dataset_root: ZfsDatasetRoot,
}

impl<'de> Deserialize<'de> for PreparedStorageState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PreparedStorageStateWire::deserialize(deserializer)?;
        Self::try_new(wire.pool, wire.origin, wire.dataset_root).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PreparedStorageStateError {
    #[error("invalid Ployz ZFS dataset root: {value}")]
    InvalidDatasetRoot { value: String },
    #[error("prepared pool {pool:?} does not own dataset root {dataset_root:?}")]
    PoolDatasetMismatch {
        pool: ZfsPoolName,
        dataset_root: ZfsDatasetRoot,
    },
    #[error("invalid owned image identity for pool {pool:?}: {}", backing_file.display())]
    InvalidOwnedImage {
        pool: ZfsPoolName,
        backing_file: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageEffectFailure {
    #[error("ZFS preparation is unsupported on this host profile")]
    UnsupportedPlatform,
    #[error("ZFS installation or module loading failed: {message}")]
    Installation { message: String },
    #[error("failed to list imported ZFS pools: {message}")]
    PoolList { message: String },
    #[error("multiple imported ZFS pools require an explicit selection: {candidates:?}")]
    AmbiguousPools { candidates: Vec<ZfsPoolName> },
    #[error("explicit ZFS pool {pool:?} is not imported")]
    ExplicitPoolAbsent { pool: ZfsPoolName },
    #[error("Ployz sparse image pool preparation failed: {message}")]
    SparsePool { message: String },
    #[error("ZFS property or dataset effect failed: {message}")]
    Dataset { message: String },
    #[error("prepared storage state is unavailable: {message}")]
    PreparedStateUnavailable { message: String },
    #[error("prepared storage state does not match observed ZFS state: {message}")]
    PreparedStateMismatch { message: String },
    #[error("ZFS fact output is invalid: {message}")]
    GatherParse { message: String },
    #[error("quota shrink is forbidden for {dataset:?}: current={current} requested={requested}")]
    QuotaShrink {
        dataset: DatasetName,
        current: u64,
        requested: u64,
    },
    #[error("quota admission exceeds available capacity")]
    QuotaCapacityExceeded {
        total_bytes: u64,
        provisioned_used_bytes: u64,
        free_bytes: u64,
        required_headroom_bytes: u64,
        requested_total_bytes: u64,
    },
    #[error("destructive ZFS effect refused: {message}")]
    DestructiveEffect { message: String },
    #[error("storage preparation exceeded its operation budget")]
    OperationTimedOut,
    #[error("storage preparation process failed: {message}")]
    ProcessFailed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineStoragePreparationEvidence {
    Completed {
        operation_id: OperationId,
        prepared: PreparedStorageState,
    },
    Failed {
        operation_id: OperationId,
        failure: StorageEffectFailure,
    },
}

impl MachineStoragePreparationEvidence {
    #[must_use]
    pub const fn operation_id(&self) -> &OperationId {
        match self {
            Self::Completed { operation_id, .. } | Self::Failed { operation_id, .. } => {
                operation_id
            }
        }
    }
}

/// One operation-scoped preparation evidence file. This is the shared source
/// for path construction and operation-id validation on both sides of the
/// privileged process boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageOperationEvidenceFile {
    operation_id: OperationId,
    path: PathBuf,
}

impl StorageOperationEvidenceFile {
    #[must_use]
    pub fn in_state_directory(state_directory: &Path, operation_id: OperationId) -> Self {
        Self::in_evidence_directory(
            &state_directory.join(STORAGE_OPERATION_EVIDENCE_DIRECTORY),
            operation_id,
        )
    }

    #[must_use]
    pub fn in_evidence_directory(directory: &Path, operation_id: OperationId) -> Self {
        let path = directory.join(format!("{}.json", operation_id.as_str()));
        Self { operation_id, path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        self.path
            .parent()
            .expect("evidence path always has a parent")
    }

    pub fn validate<'a>(
        &self,
        evidence: &'a MachineStoragePreparationEvidence,
    ) -> Result<&'a MachineStoragePreparationEvidence, StorageEffectFailure> {
        if evidence.operation_id() == &self.operation_id {
            Ok(evidence)
        } else {
            Err(StorageEffectFailure::ProcessFailed {
                message: "storage preparation evidence names another operation".to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_state_deserialization_rejects_inconsistent_provenance() {
        let error = serde_json::from_str::<PreparedStorageState>(
            r#"{"pool":"tank","origin":{"kind":"owned_image","backing_file":"/var/lib/ployz/zfs/ployz.img"},"dataset_root":"tank/ployz/volumes"}"#,
        )
        .expect_err("owned state must use the Ployz pool");
        assert!(error.to_string().contains("invalid owned image identity"));
    }
}
