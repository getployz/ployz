//! Exact machine substrate manifest selected by an accepting cluster.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::install::{ExactPloyzVersion, InstallArtifactSpec};

use super::JoinAcceptanceValidationError;

/// Compiler-checked identity for every artifact in the accepted machine substrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinArtifactKind {
    Ployzd,
    EbpfBytecode,
    EbpfCtl,
    Corrosion,
    CorrosionSchema,
}

impl JoinArtifactKind {
    pub const ALL: [Self; 5] = [
        Self::Ployzd,
        Self::EbpfBytecode,
        Self::EbpfCtl,
        Self::Corrosion,
        Self::CorrosionSchema,
    ];

    const fn install_path(self) -> &'static str {
        match self {
            Self::Ployzd => "/usr/local/bin/ployzd",
            Self::EbpfBytecode => "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            Self::EbpfCtl => "/usr/local/bin/ployz-ebpf-ctl",
            Self::Corrosion => "/usr/local/bin/corrosion",
            Self::CorrosionSchema => "/usr/local/lib/ployz/corrosion-schema-v1.sql",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JoinMachineSubstrateWire {
    ployz_version: ExactPloyzVersion,
    corrosion_version: String,
    artifacts: Vec<InstallArtifactSpec>,
}

/// Exact machine substrate selected by the accepting cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    try_from = "JoinMachineSubstrateWire",
    into = "JoinMachineSubstrateWire"
)]
pub struct JoinMachineSubstrate {
    ployz_version: ExactPloyzVersion,
    corrosion_version: String,
    artifacts: Vec<InstallArtifactSpec>,
}

impl JoinMachineSubstrate {
    pub fn try_new(
        ployz_version: ExactPloyzVersion,
        corrosion_version: impl Into<String>,
        artifacts: Vec<InstallArtifactSpec>,
    ) -> Result<Self, JoinAcceptanceValidationError> {
        let substrate = Self {
            ployz_version,
            corrosion_version: corrosion_version.into(),
            artifacts,
        };
        substrate.validate()?;
        Ok(substrate)
    }

    #[must_use]
    pub const fn ployz_version(&self) -> &ExactPloyzVersion {
        &self.ployz_version
    }

    #[must_use]
    pub fn corrosion_version(&self) -> &str {
        &self.corrosion_version
    }

    #[must_use]
    pub fn artifacts(&self) -> &[InstallArtifactSpec] {
        &self.artifacts
    }

    /// Returns every required artifact paired with its exhaustive semantic kind.
    #[must_use]
    pub fn artifacts_by_kind(
        &self,
    ) -> impl ExactSizeIterator<Item = (JoinArtifactKind, &InstallArtifactSpec)> {
        JoinArtifactKind::ALL.into_iter().map(|kind| {
            let path = kind.install_path();
            let Some(artifact) = self
                .artifacts
                .iter()
                .find(|artifact| artifact.install_path.as_str() == path)
            else {
                unreachable!("validated substrate contains every required artifact")
            };
            (kind, artifact)
        })
    }

    pub(super) fn validate(&self) -> Result<(), JoinAcceptanceValidationError> {
        if self.corrosion_version.trim().is_empty() {
            return Err(JoinAcceptanceValidationError::MissingCorrosionVersion);
        }

        let mut destinations = BTreeSet::new();
        // Only required destinations are installed; extra metadata is inert.
        for artifact in &self.artifacts {
            let destination = artifact.install_path.as_str();
            if !destinations.insert(destination) {
                return Err(JoinAcceptanceValidationError::DuplicateInstallArtifact {
                    install_path: destination.to_owned(),
                });
            }
        }
        for required in JoinArtifactKind::ALL.map(JoinArtifactKind::install_path) {
            if !destinations.contains(required) {
                return Err(JoinAcceptanceValidationError::MissingInstallArtifact {
                    install_path: required.to_owned(),
                });
            }
        }
        Ok(())
    }
}

impl TryFrom<JoinMachineSubstrateWire> for JoinMachineSubstrate {
    type Error = JoinAcceptanceValidationError;

    fn try_from(wire: JoinMachineSubstrateWire) -> Result<Self, Self::Error> {
        Self::try_new(wire.ployz_version, wire.corrosion_version, wire.artifacts)
    }
}

impl From<JoinMachineSubstrate> for JoinMachineSubstrateWire {
    fn from(substrate: JoinMachineSubstrate) -> Self {
        Self {
            ployz_version: substrate.ployz_version,
            corrosion_version: substrate.corrosion_version,
            artifacts: substrate.artifacts,
        }
    }
}
