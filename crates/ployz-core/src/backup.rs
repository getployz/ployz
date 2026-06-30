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
    MachineLocalCache,
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
        Self::MachineLocalCache,
    ];

    #[must_use]
    pub const fn policy(self) -> BackupPolicy {
        match self {
            Self::CoreStateKv
            | Self::OperationStateKv
            | Self::ObservationStateKv
            | Self::LockStateKv
            | Self::BackupManifest
            | Self::NatsCredentials
            | Self::NatsServerConfig
            | Self::PloyzDomainConfig
            | Self::OperationEventStreams => BackupPolicy::Included,
            Self::DockerImages
            | Self::ApplicationVolumes
            | Self::ContainerRuntimeState
            | Self::MachineLocalCache => BackupPolicy::Excluded,
        }
    }

    #[must_use]
    pub const fn current_bundle_policy(self) -> BackupPolicy {
        match self {
            Self::CoreStateKv | Self::LockStateKv | Self::BackupManifest => BackupPolicy::Included,
            Self::OperationStateKv
            | Self::ObservationStateKv
            | Self::NatsCredentials
            | Self::NatsServerConfig
            | Self::PloyzDomainConfig
            | Self::OperationEventStreams
            | Self::DockerImages
            | Self::ApplicationVolumes
            | Self::ContainerRuntimeState
            | Self::MachineLocalCache => BackupPolicy::Excluded,
        }
    }

    #[must_use]
    pub const fn scope_entry(self) -> BackupScopeEntry {
        BackupScopeEntry {
            item: self,
            policy: self.policy(),
        }
    }

    #[must_use]
    pub const fn current_bundle_scope_entry(self) -> BackupScopeEntry {
        BackupScopeEntry {
            item: self,
            policy: self.current_bundle_policy(),
        }
    }
}

pub fn control_plane_backup_scope() -> impl Iterator<Item = BackupScopeEntry> {
    BackupItem::ALL.into_iter().map(BackupItem::scope_entry)
}

pub fn current_control_plane_bundle_scope() -> impl Iterator<Item = BackupScopeEntry> {
    BackupItem::ALL
        .into_iter()
        .map(BackupItem::current_bundle_scope_entry)
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
    WaitForMachineReconnects,
    RebuildObservationsFromReality,
}

impl RestoreStep {
    pub const ALL: [Self; 4] = [
        Self::RecreateControlPlaneAuthority,
        Self::RestoreJetStreamState,
        Self::WaitForMachineReconnects,
        Self::RebuildObservationsFromReality,
    ];
}

pub fn single_core_restore_contract() -> impl Iterator<Item = RestoreStep> {
    RestoreStep::ALL.into_iter()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupTarget {
    S3 {
        bucket: String,
        key_prefix: String,
        region: String,
        endpoint_url: Option<String>,
        addressing_style: S3AddressingStyle,
    },
}

impl BackupTarget {
    #[must_use]
    pub fn s3(target: S3BackupTarget) -> Self {
        let S3BackupTarget {
            bucket,
            key_prefix,
            region,
            endpoint_url,
            addressing_style,
        } = target;
        Self::S3 {
            bucket,
            key_prefix,
            region,
            endpoint_url,
            addressing_style,
        }
    }

    pub fn validate_create(&self) -> Result<(), BackupTargetValidationError> {
        match self {
            Self::S3 {
                bucket,
                key_prefix,
                region,
                endpoint_url,
                ..
            } => validate_s3_backup_target(bucket, key_prefix, region, endpoint_url.as_deref()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct S3BackupTarget {
    pub bucket: String,
    pub key_prefix: String,
    pub region: String,
    pub endpoint_url: Option<String>,
    pub addressing_style: S3AddressingStyle,
}

impl S3BackupTarget {
    #[must_use]
    pub fn new(
        bucket: impl Into<String>,
        key_prefix: impl Into<String>,
        region: impl Into<String>,
        endpoint_url: Option<String>,
        addressing_style: S3AddressingStyle,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            key_prefix: key_prefix.into(),
            region: region.into(),
            endpoint_url,
            addressing_style,
        }
    }

    pub fn validate_create(&self) -> Result<(), BackupTargetValidationError> {
        validate_s3_backup_target(
            &self.bucket,
            &self.key_prefix,
            &self.region,
            self.endpoint_url.as_deref(),
        )
    }
}

fn validate_s3_backup_target(
    bucket: &str,
    key_prefix: &str,
    region: &str,
    endpoint_url: Option<&str>,
) -> Result<(), BackupTargetValidationError> {
    validate_backup_target_text(BackupTargetValidationField::Bucket, bucket)?;
    validate_backup_target_text(BackupTargetValidationField::KeyPrefix, key_prefix)?;
    validate_backup_target_text(BackupTargetValidationField::Region, region)?;
    if let Some(endpoint_url) = endpoint_url {
        validate_backup_target_text(BackupTargetValidationField::EndpointUrl, endpoint_url)?;
    }
    Ok(())
}

fn validate_backup_target_text(
    field: BackupTargetValidationField,
    value: &str,
) -> Result<(), BackupTargetValidationError> {
    if value.trim().is_empty() {
        return Err(BackupTargetValidationError {
            field,
            failure: BackupTargetValidationFailure::Empty,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
#[error("{} {}", field.wire_name(), failure.message())]
pub struct BackupTargetValidationError {
    pub field: BackupTargetValidationField,
    pub failure: BackupTargetValidationFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BackupTargetValidationField {
    Bucket,
    KeyPrefix,
    Region,
    EndpointUrl,
}

impl BackupTargetValidationField {
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Bucket => "bucket",
            Self::KeyPrefix => "key_prefix",
            Self::Region => "region",
            Self::EndpointUrl => "endpoint_url",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum BackupTargetValidationFailure {
    Empty,
}

impl BackupTargetValidationFailure {
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Empty => "must not be empty",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupRestoreSource {
    S3 {
        bucket: String,
        manifest_key: String,
        region: String,
        endpoint_url: Option<String>,
        addressing_style: S3AddressingStyle,
    },
}

impl BackupRestoreSource {
    #[must_use]
    pub fn s3(source: S3BackupRestoreSource) -> Self {
        let S3BackupRestoreSource {
            bucket,
            manifest_key,
            region,
            endpoint_url,
            addressing_style,
        } = source;
        Self::S3 {
            bucket,
            manifest_key,
            region,
            endpoint_url,
            addressing_style,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct S3BackupRestoreSource {
    pub bucket: String,
    pub manifest_key: String,
    pub region: String,
    pub endpoint_url: Option<String>,
    pub addressing_style: S3AddressingStyle,
}

impl S3BackupRestoreSource {
    #[must_use]
    pub fn new(
        bucket: impl Into<String>,
        manifest_key: impl Into<String>,
        region: impl Into<String>,
        endpoint_url: Option<String>,
        addressing_style: S3AddressingStyle,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            manifest_key: manifest_key.into(),
            region: region.into(),
            endpoint_url,
            addressing_style,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum S3AddressingStyle {
    VirtualHosted,
    Path,
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
    pub fn current_control_plane_kv_only() -> Self {
        Self {
            format_version: BackupManifestVersion::V1,
            scope: current_control_plane_bundle_scope().collect(),
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
    pub location: BackupArtifactLocation,
    pub kind: BackupArtifactKind,
    pub byte_count: u64,
    pub sha256_digest: String,
}

impl BackupArtifact {
    #[must_use]
    pub fn new(
        location: BackupArtifactLocation,
        kind: BackupArtifactKind,
        byte_count: u64,
        sha256_digest: impl Into<String>,
    ) -> Self {
        Self {
            location,
            kind,
            byte_count,
            sha256_digest: sha256_digest.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupArtifactLocation {
    S3 {
        bucket: String,
        key: String,
        region: String,
        endpoint_url: Option<String>,
        addressing_style: S3AddressingStyle,
    },
}

impl BackupArtifactLocation {
    #[must_use]
    pub fn s3(
        bucket: impl Into<String>,
        key: impl Into<String>,
        region: impl Into<String>,
        endpoint_url: Option<String>,
        addressing_style: S3AddressingStyle,
    ) -> Self {
        Self::S3 {
            bucket: bucket.into(),
            key: key.into(),
            region: region.into(),
            endpoint_url,
            addressing_style,
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
