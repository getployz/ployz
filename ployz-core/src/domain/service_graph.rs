//! Validated Service Volume and Config definition-plus-mount graphs.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::{ConfigMount, ConfigSpec, ServiceMount, ServiceVolume};
use crate::ServiceVolumeReference;

/// Service Volume definitions together with the mounts that refer to them.
///
/// Duplicate references and dangling mounts are rejected. Unused definitions,
/// repeated mounts, and aliases that share a Docker Volume name stay legal.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ServiceVolumeGraph {
    volumes: Vec<ServiceVolume>,
    mounts: Vec<ServiceMount>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceVolumeGraphError {
    #[error("duplicate Service Volume Reference: {reference}")]
    DuplicateVolumeReference { reference: ServiceVolumeReference },
    #[error("mount references an undeclared Service Volume: {reference}")]
    UnknownVolumeReference { reference: ServiceVolumeReference },
}

impl ServiceVolumeGraph {
    /// Build a graph from Volume definitions and the mounts that refer to them.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceVolumeGraphError`] when two definitions share a
    /// reference or a mount names a reference that was not declared.
    pub fn parse(
        volumes: Vec<ServiceVolume>,
        mounts: Vec<ServiceMount>,
    ) -> Result<Self, ServiceVolumeGraphError> {
        let mut references = BTreeSet::new();
        for volume in &volumes {
            if !references.insert(&volume.reference) {
                return Err(ServiceVolumeGraphError::DuplicateVolumeReference {
                    reference: volume.reference.clone(),
                });
            }
        }
        for mount in &mounts {
            if !references.contains(&mount.volume) {
                return Err(ServiceVolumeGraphError::UnknownVolumeReference {
                    reference: mount.volume.clone(),
                });
            }
        }
        Ok(Self { volumes, mounts })
    }

    #[must_use]
    pub fn volumes(&self) -> &[ServiceVolume] {
        &self.volumes
    }

    #[must_use]
    pub fn mounts(&self) -> &[ServiceMount] {
        &self.mounts
    }
}

impl<'de> Deserialize<'de> for ServiceVolumeGraph {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Data {
            #[serde(default)]
            volumes: Vec<ServiceVolume>,
            #[serde(default)]
            mounts: Vec<ServiceMount>,
        }

        let data = Data::deserialize(deserializer)?;
        Self::parse(data.volumes, data.mounts).map_err(D::Error::custom)
    }
}

/// Config definitions together with the mounts that refer to them.
///
/// Duplicate names and dangling mounts are rejected. Unused definitions and
/// repeated mounts stay legal.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ServiceConfigGraph {
    configs: Vec<ConfigSpec>,
    mounts: Vec<ConfigMount>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceConfigGraphError {
    #[error("duplicate config name: {name}")]
    DuplicateConfigName { name: String },
    #[error("config mount references an undeclared config: {name}")]
    UnknownConfigName { name: String },
}

impl ServiceConfigGraph {
    /// Build a graph from Config definitions and the mounts that refer to them.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceConfigGraphError`] when two definitions share a name or
    /// a mount names a config that was not declared.
    pub fn parse(
        configs: Vec<ConfigSpec>,
        mounts: Vec<ConfigMount>,
    ) -> Result<Self, ServiceConfigGraphError> {
        let mut names = BTreeSet::new();
        for config in &configs {
            if !names.insert(config.name.as_str()) {
                return Err(ServiceConfigGraphError::DuplicateConfigName {
                    name: config.name.clone(),
                });
            }
        }
        for mount in &mounts {
            if !names.contains(mount.config_name.as_str()) {
                return Err(ServiceConfigGraphError::UnknownConfigName {
                    name: mount.config_name.clone(),
                });
            }
        }
        Ok(Self { configs, mounts })
    }

    #[must_use]
    pub fn configs(&self) -> &[ConfigSpec] {
        &self.configs
    }

    #[must_use]
    pub fn mounts(&self) -> &[ConfigMount] {
        &self.mounts
    }
}

impl<'de> Deserialize<'de> for ServiceConfigGraph {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Data {
            #[serde(default)]
            configs: Vec<ConfigSpec>,
            #[serde(default)]
            mounts: Vec<ConfigMount>,
        }

        let data = Data::deserialize(deserializer)?;
        Self::parse(data.configs, data.mounts).map_err(D::Error::custom)
    }
}
