//! Product-level control-plane backup scope.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BackupPolicy {
    Included,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BackupItem {
    CoreStateKv,
    OperationStateKv,
    ObservationStateKv,
    LockStateKv,
    BackupManifest,
    NatsCredentials,
    NatsServerConfig,
    PloyzDomainConfig,
    OperationEventStreams,
    DockerImages,
    ApplicationVolumes,
    ContainerRuntimeState,
    NodeLocalCache,
}

impl BackupItem {
    pub const ALL: [Self; 13] = [
        Self::CoreStateKv,
        Self::OperationStateKv,
        Self::ObservationStateKv,
        Self::LockStateKv,
        Self::BackupManifest,
        Self::NatsCredentials,
        Self::NatsServerConfig,
        Self::PloyzDomainConfig,
        Self::OperationEventStreams,
        Self::DockerImages,
        Self::ApplicationVolumes,
        Self::ContainerRuntimeState,
        Self::NodeLocalCache,
    ];

    #[must_use]
    pub const fn policy(self) -> BackupPolicy {
        match self {
            Self::CoreStateKv
            | Self::OperationStateKv
            | Self::ObservationStateKv
            | Self::LockStateKv => BackupPolicy::Included,
            Self::BackupManifest
            | Self::NatsCredentials
            | Self::NatsServerConfig
            | Self::PloyzDomainConfig
            | Self::OperationEventStreams
            | Self::DockerImages
            | Self::ApplicationVolumes
            | Self::ContainerRuntimeState
            | Self::NodeLocalCache => BackupPolicy::Excluded,
        }
    }

    #[must_use]
    pub const fn scope_entry(self) -> BackupScopeEntry {
        BackupScopeEntry {
            item: self,
            policy: self.policy(),
        }
    }
}

pub fn control_plane_backup_scope() -> impl Iterator<Item = BackupScopeEntry> {
    BackupItem::ALL.into_iter().map(BackupItem::scope_entry)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BackupScopeEntry {
    pub item: BackupItem,
    pub policy: BackupPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum RestoreStep {
    RecreateControlPlaneAuthority,
    RestoreJetStreamState,
    WaitForNodeReconnects,
    RebuildObservationsFromReality,
}

impl RestoreStep {
    pub const ALL: [Self; 4] = [
        Self::RecreateControlPlaneAuthority,
        Self::RestoreJetStreamState,
        Self::WaitForNodeReconnects,
        Self::RebuildObservationsFromReality,
    ];
}

pub fn single_core_restore_contract() -> impl Iterator<Item = RestoreStep> {
    RestoreStep::ALL.into_iter()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format_version: BackupManifestVersion,
    pub scope: Vec<BackupScopeEntry>,
    pub restore_contract: Vec<RestoreStep>,
    pub artifacts: Vec<BackupArtifact>,
}

impl BackupManifest {
    #[must_use]
    pub fn single_core_control_plane() -> Self {
        Self {
            format_version: BackupManifestVersion::V1,
            scope: control_plane_backup_scope().collect(),
            restore_contract: single_core_restore_contract().collect(),
            artifacts: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_artifact(mut self, artifact: BackupArtifact) -> Self {
        self.artifacts.push(artifact);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BackupArtifact {
    pub bucket: String,
    pub object_name: String,
    pub kind: BackupArtifactKind,
    pub byte_count: u64,
    pub digest: String,
}

impl BackupArtifact {
    #[must_use]
    pub fn new(
        bucket: impl Into<String>,
        object_name: impl Into<String>,
        kind: BackupArtifactKind,
        byte_count: u64,
        digest: impl Into<String>,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            object_name: object_name.into(),
            kind,
            byte_count,
            digest: digest.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BackupArtifactKind {
    ControlPlaneBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BackupBundle {
    pub format_version: BackupManifestVersion,
    pub control_plane: ControlPlaneKvSnapshot,
}

impl BackupBundle {
    #[must_use]
    pub fn new(control_plane: ControlPlaneKvSnapshot) -> Self {
        Self {
            format_version: BackupManifestVersion::V1,
            control_plane,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneKvSnapshot {
    pub format_version: BackupManifestVersion,
    pub buckets: Vec<KvBucketSnapshot>,
}

impl ControlPlaneKvSnapshot {
    #[must_use]
    pub fn new(buckets: Vec<KvBucketSnapshot>) -> Self {
        Self {
            format_version: BackupManifestVersion::V1,
            buckets,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct KvBucketSnapshot {
    pub name: String,
    pub entries: Vec<KvEntrySnapshot>,
}

impl KvBucketSnapshot {
    #[must_use]
    pub fn new(name: impl Into<String>, entries: Vec<KvEntrySnapshot>) -> Self {
        Self {
            name: name.into(),
            entries,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct KvEntrySnapshot {
    pub key: String,
    pub revision: u64,
    pub value: Vec<u8>,
}

impl KvEntrySnapshot {
    #[must_use]
    pub fn new(key: impl Into<String>, revision: u64, value: Vec<u8>) -> Self {
        Self {
            key: key.into(),
            revision,
            value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BackupManifestVersion {
    V1,
}
