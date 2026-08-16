//! Validated Service Volume and Config definition-plus-mount graphs.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::{
    ConfigMount, ConfigSpec, RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig,
    ServiceMount, ServiceVolume, UpdateConfig,
};
use crate::{ServiceId, ServiceVolumeReference};

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

    /// Consume the graph and return its Volume definitions and mounts.
    #[must_use]
    pub fn into_parts(self) -> (Vec<ServiceVolume>, Vec<ServiceMount>) {
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

    /// Consume the graph and return its Config definitions and mounts.
    #[must_use]
    pub fn into_parts(self) -> (Vec<ConfigSpec>, Vec<ConfigMount>) {
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

impl RequestedServiceSpec {
    /// Validate this spec's Volume definitions together with its mounts.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceVolumeGraphError`] when the parallel arrays are not a
    /// valid Volume graph.
    pub fn to_volume_graph(&self) -> Result<ServiceVolumeGraph, ServiceVolumeGraphError> {
        volume_graph(&self.volumes, &self.mounts)
    }

    /// Validate this spec's Config definitions together with its mounts.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceConfigGraphError`] when the parallel arrays are not a
    /// valid Config graph.
    pub fn to_config_graph(&self) -> Result<ServiceConfigGraph, ServiceConfigGraphError> {
        config_graph(&self.configs, &self.container.config_mounts)
    }

    /// Resolve this spec after validating its Volume and Config graphs.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceSpecGraphError`] when either graph is invalid.
    pub fn to_resolved(
        &self,
        service_id: ServiceId,
        update: ResolvedUpdateConfig,
    ) -> Result<ResolvedServiceSpec, ServiceSpecGraphError> {
        self.to_volume_graph()?;
        self.to_config_graph()?;
        Ok(ResolvedServiceSpec {
            service_id,
            name: self.name.clone(),
            mode: self.mode.clone(),
            container: self.container.clone(),
            placement: self.placement.clone(),
            ports: self.ports.clone(),
            volumes: self.volumes.clone(),
            mounts: self.mounts.clone(),
            configs: self.configs.clone(),
            pre_deploy: self.pre_deploy.clone(),
            caddy_config: self.caddy_config.clone(),
            update,
        })
    }
}

impl ResolvedServiceSpec {
    /// Validate this spec's Volume definitions together with its mounts.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceVolumeGraphError`] when the parallel arrays are not a
    /// valid Volume graph.
    pub fn to_volume_graph(&self) -> Result<ServiceVolumeGraph, ServiceVolumeGraphError> {
        volume_graph(&self.volumes, &self.mounts)
    }

    /// Validate this spec's Config definitions together with its mounts.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceConfigGraphError`] when the parallel arrays are not a
    /// valid Config graph.
    pub fn to_config_graph(&self) -> Result<ServiceConfigGraph, ServiceConfigGraphError> {
        config_graph(&self.configs, &self.container.config_mounts)
    }

    /// Rebuild a requested spec after validating this spec's graphs.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceSpecGraphError`] when either graph is invalid.
    pub fn to_requested(&self) -> Result<RequestedServiceSpec, ServiceSpecGraphError> {
        self.to_volume_graph()?;
        self.to_config_graph()?;
        Ok(RequestedServiceSpec {
            name: self.name.clone(),
            mode: self.mode.clone(),
            container: self.container.clone(),
            placement: self.placement.clone(),
            ports: self.ports.clone(),
            volumes: self.volumes.clone(),
            mounts: self.mounts.clone(),
            configs: self.configs.clone(),
            pre_deploy: self.pre_deploy.clone(),
            caddy_config: self.caddy_config.clone(),
            update: UpdateConfig {
                order: Some(self.update.order),
                monitor_millis: self.update.monitor_millis,
            },
        })
    }
}

fn volume_graph(
    volumes: &[ServiceVolume],
    mounts: &[ServiceMount],
) -> Result<ServiceVolumeGraph, ServiceVolumeGraphError> {
    ServiceVolumeGraph::parse(volumes.to_vec(), mounts.to_vec())
}

fn config_graph(
    configs: &[ConfigSpec],
    mounts: &[ConfigMount],
) -> Result<ServiceConfigGraph, ServiceConfigGraphError> {
    ServiceConfigGraph::parse(configs.to_vec(), mounts.to_vec())
}
