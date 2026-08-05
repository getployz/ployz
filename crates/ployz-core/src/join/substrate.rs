//! Exact machine substrate manifest selected by an accepting cluster.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::install::{ExactPloyzVersion, InstallArtifactSpec};

use super::JoinAcceptanceValidationError;

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
    pub const REQUIRED_INSTALL_PATHS: [&'static str; 6] = [
        "/usr/local/bin/ployzd",
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        "/usr/local/bin/ployz-ebpf-ctl",
        "/usr/local/bin/corrosion",
        "/usr/local/lib/ployz/corrosion-schema-v1.sql",
        "/usr/local/bin/railpack",
    ];

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

    pub(super) fn validate(&self) -> Result<(), JoinAcceptanceValidationError> {
        if self.corrosion_version.trim().is_empty() {
            return Err(JoinAcceptanceValidationError::MissingCorrosionVersion);
        }

        let mut destinations = BTreeSet::new();
        for artifact in &self.artifacts {
            let destination = artifact.install_path.as_str();
            if !Self::REQUIRED_INSTALL_PATHS.contains(&destination) {
                return Err(JoinAcceptanceValidationError::UnknownInstallArtifact {
                    install_path: destination.to_owned(),
                });
            }
            if !destinations.insert(destination) {
                return Err(JoinAcceptanceValidationError::DuplicateInstallArtifact {
                    install_path: destination.to_owned(),
                });
            }
        }
        for required in Self::REQUIRED_INSTALL_PATHS {
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
