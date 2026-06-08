//! Product-level control-plane backup scope.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupPolicy {
    Included,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupItem {
    JetStreamDataDirectory,
    NatsCredentials,
    NatsServerConfig,
    PloyzDomainConfig,
    BackupManifest,
    DockerImages,
    ApplicationVolumes,
    ContainerRuntimeState,
    NodeLocalCache,
}

impl BackupItem {
    pub const ALL: [Self; 9] = [
        Self::JetStreamDataDirectory,
        Self::NatsCredentials,
        Self::NatsServerConfig,
        Self::PloyzDomainConfig,
        Self::BackupManifest,
        Self::DockerImages,
        Self::ApplicationVolumes,
        Self::ContainerRuntimeState,
        Self::NodeLocalCache,
    ];

    #[must_use]
    pub const fn policy(self) -> BackupPolicy {
        match self {
            Self::JetStreamDataDirectory
            | Self::NatsCredentials
            | Self::NatsServerConfig
            | Self::PloyzDomainConfig
            | Self::BackupManifest => BackupPolicy::Included,
            Self::DockerImages
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupScopeEntry {
    pub item: BackupItem,
    pub policy: BackupPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreStep {
    RecreateControlPlaneAuthority,
    RestoreNatsCredentialsAndConfig,
    RestorePloyzDomainConfig,
    RestoreJetStreamState,
    WaitForNodeReconnects,
    RebuildObservationsFromReality,
}

impl RestoreStep {
    pub const ALL: [Self; 6] = [
        Self::RecreateControlPlaneAuthority,
        Self::RestoreNatsCredentialsAndConfig,
        Self::RestorePloyzDomainConfig,
        Self::RestoreJetStreamState,
        Self::WaitForNodeReconnects,
        Self::RebuildObservationsFromReality,
    ];
}

pub fn single_core_restore_contract() -> impl Iterator<Item = RestoreStep> {
    RestoreStep::ALL.into_iter()
}
