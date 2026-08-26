//! Validated Service Volume and Config definition-plus-mount graphs.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::{
    ConfigMount, ConfigSpec, PROVISIONED_VOLUME_DRIVER, ServiceMount, ServiceVolume, VolumeSource,
};
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
    #[error(
        "ordinary Service Volume {reference} cannot use reserved Docker driver '{PROVISIONED_VOLUME_DRIVER}'"
    )]
    /// An ordinary named Volume selected the driver reserved for Provisioned Volumes.
    ReservedProvisionedVolumeDriver { reference: ServiceVolumeReference },
}

impl ServiceVolumeGraph {
    /// Build a graph from Volume definitions and the mounts that refer to them.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceVolumeGraphError`] when two definitions share a
    /// reference, a mount names a reference that was not declared, or an
    /// ordinary named Volume selects the reserved Provisioned Volume driver.
    pub fn parse(
        volumes: Vec<ServiceVolume>,
        mounts: Vec<ServiceMount>,
    ) -> Result<Self, ServiceVolumeGraphError> {
        let mut references = BTreeSet::new();
        for volume in &volumes {
            if matches!(
                &volume.source,
                VolumeSource::Named { driver: Some(driver), .. }
                    if driver.name == PROVISIONED_VOLUME_DRIVER
            ) {
                return Err(ServiceVolumeGraphError::ReservedProvisionedVolumeDriver {
                    reference: volume.reference.clone(),
                });
            }
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

    /// Volume definition for a mount that belongs to this graph.
    ///
    /// # Panics
    ///
    /// Panics if `mount` names a reference that is not in this graph. [`parse`](Self::parse)
    /// rejects dangling mounts, so that is a programmer bug.
    #[must_use]
    pub fn volume_for(&self, mount: &ServiceMount) -> &ServiceVolume {
        self.volumes
            .iter()
            .find(|volume| volume.reference == mount.volume)
            .expect("ServiceVolumeGraph::parse rejects dangling mounts")
    }

    /// Volume definitions used by mounts, including repeated mounts of one definition.
    pub fn mounted_volumes(&self) -> impl Iterator<Item = &ServiceVolume> {
        self.mounts.iter().map(|mount| self.volume_for(mount))
    }

    /// Mounted Provisioned Volume definitions, including repeated mounts.
    pub fn mounted_provisioned_volumes(&self) -> impl Iterator<Item = &ServiceVolume> {
        self.mounted_volumes()
            .filter(|volume| matches!(volume.source, VolumeSource::Provisioned { .. }))
    }

    /// Whether any mounted source requires Provisioned storage capability.
    #[must_use]
    pub fn has_mounted_provisioned_volume(&self) -> bool {
        self.mounted_provisioned_volumes().next().is_some()
    }

    pub(crate) fn into_parts(self) -> (Vec<ServiceVolume>, Vec<ServiceMount>) {
        (self.volumes, self.mounts)
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

    /// Config definition for a mount that belongs to this graph.
    ///
    /// # Panics
    ///
    /// Panics if `mount` names a config that is not in this graph. [`parse`](Self::parse)
    /// rejects dangling mounts, so that is a programmer bug.
    #[must_use]
    pub fn config_for(&self, mount: &ConfigMount) -> &ConfigSpec {
        self.configs
            .iter()
            .find(|config| config.name == mount.config_name)
            .expect("ServiceConfigGraph::parse rejects dangling mounts")
    }

    pub(crate) fn into_parts(self) -> (Vec<ConfigSpec>, Vec<ConfigMount>) {
        (self.configs, self.mounts)
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

/// Failure to read a Service spec as validated Volume and Config graphs.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ServiceSpecGraphError {
    #[error(transparent)]
    Volume(#[from] ServiceVolumeGraphError),
    #[error(transparent)]
    Config(#[from] ServiceConfigGraphError),
}
