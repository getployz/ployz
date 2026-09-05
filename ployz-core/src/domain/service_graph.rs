//! Validated Service Volume and Config definition-plus-mount graphs.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::{ConfigMount, ConfigSpec, ServiceMount, ServiceVolume, VolumeSource};
use crate::{DockerVolumeName, ServiceVolumeReference};

/// Service Volume definitions together with the mounts that refer to them.
///
/// Duplicate references and dangling mounts are rejected. Unused definitions,
/// repeated mounts, and compatible aliases for one Docker Volume stay legal.
/// [`ServiceMountGraph`] additionally admits destinations across Volumes and Configs.
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
    /// Two references describe incompatible sources for one physical Docker Volume.
    #[error("incompatible Service Volume aliases use Docker Volume {name}")]
    IncompatibleVolumeAliases { name: DockerVolumeName },
    #[error("resolved Service Volume {reference} has no Project scope")]
    UnscopedVolume { reference: ServiceVolumeReference },
}

impl ServiceVolumeGraph {
    /// Build a graph from Volume definitions and the mounts that refer to them.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceVolumeGraphError`] when two definitions share a
    /// reference, a mount names a reference that was not declared, or an
    /// alias for one Docker Volume declares an incompatible source shape.
    pub fn parse(
        volumes: Vec<ServiceVolume>,
        mounts: Vec<ServiceMount>,
    ) -> Result<Self, ServiceVolumeGraphError> {
        let mut references = BTreeSet::new();
        let mut docker_volumes = BTreeMap::<&DockerVolumeName, &VolumeSource>::new();
        for volume in &volumes {
            if !references.insert(&volume.reference) {
                return Err(ServiceVolumeGraphError::DuplicateVolumeReference {
                    reference: volume.reference.clone(),
                });
            }
            let Some(name) = volume.source.docker_volume_name() else {
                continue;
            };
            if docker_volumes
                .insert(name, &volume.source)
                .is_some_and(|existing| existing != &volume.source)
            {
                return Err(ServiceVolumeGraphError::IncompatibleVolumeAliases {
                    name: name.clone(),
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

    /// Scope managed declarations and revalidate aliases after physical names change.
    ///
    /// # Errors
    ///
    /// Returns an incompatible-alias error if scoping creates a physical-name collision.
    pub fn scope_to_project(
        self,
        project: &crate::ProjectName,
    ) -> Result<Self, ServiceVolumeGraphError> {
        let (mut volumes, mounts) = self.into_parts();
        for volume in &mut volumes {
            volume.source.scope_to_project(project);
        }
        Self::parse(volumes, mounts)
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
        self.mounted_volumes().filter(|volume| {
            matches!(
                volume.source.kind(),
                crate::RawVolumeSource::Provisioned { .. }
            )
        })
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
/// repeated mounts stay legal. Destination admission belongs to [`ServiceMountGraph`].
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
    #[error("mount target resolves to root /")]
    RootMountTarget,
    #[error("duplicate effective mount target: {target}")]
    DuplicateMountTarget { target: crate::ContainerPath },
    #[error(transparent)]
    Volume(#[from] ServiceVolumeGraphError),
    #[error(transparent)]
    Config(#[from] ServiceConfigGraphError),
}

/// A graph whose managed sources all carry privately established Project scope.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedServiceVolumeGraph(ServiceVolumeGraph);

impl TryFrom<ServiceVolumeGraph> for ResolvedServiceVolumeGraph {
    type Error = ServiceVolumeGraphError;
    fn try_from(graph: ServiceVolumeGraph) -> Result<Self, Self::Error> {
        if let Some(volume) = graph
            .volumes()
            .iter()
            .find(|volume| !volume.source.is_resolved())
        {
            return Err(ServiceVolumeGraphError::UnscopedVolume {
                reference: volume.reference.clone(),
            });
        }
        Ok(Self(graph))
    }
}
impl std::ops::Deref for ResolvedServiceVolumeGraph {
    type Target = ServiceVolumeGraph;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl ResolvedServiceVolumeGraph {
    /// Preserve exact scoped source identity for an observed scale request.
    #[must_use]
    pub fn to_requested(&self) -> ServiceVolumeGraph {
        self.0.clone()
    }
    pub(crate) fn into_parts(self) -> (Vec<ServiceVolume>, Vec<ServiceMount>) {
        self.0.into_parts()
    }
}

/// Volume and Config graphs admitted together with unique effective destinations.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceMountGraph {
    volumes: ServiceVolumeGraph,
    configs: ServiceConfigGraph,
}

impl ServiceMountGraph {
    /// Admit the combined mounts and retain their canonical Linux destinations.
    ///
    /// # Errors
    /// Rejects root destinations and repeated effective destinations.
    pub fn new(
        mut volumes: ServiceVolumeGraph,
        mut configs: ServiceConfigGraph,
    ) -> Result<Self, ServiceSpecGraphError> {
        let mut destinations = BTreeSet::new();
        for mount in &mut volumes.mounts {
            mount.target = admit_target(mount.target.as_str(), &mut destinations)?;
        }
        for mount in &mut configs.mounts {
            let default = format!("/{}", mount.config_name);
            mount.target = Some(admit_target(
                mount
                    .target
                    .as_ref()
                    .map_or(default.as_str(), |target| target.as_str()),
                &mut destinations,
            )?);
        }
        Ok(Self { volumes, configs })
    }

    #[must_use]
    pub fn volume_graph(&self) -> &ServiceVolumeGraph {
        &self.volumes
    }
    #[must_use]
    pub fn config_graph(&self) -> &ServiceConfigGraph {
        &self.configs
    }

    /// Scope sources without changing the admitted destinations.
    ///
    /// # Errors
    /// Rejects physical volume aliases made incompatible by scoping.
    pub fn scope_to_project(
        self,
        project: &crate::ProjectName,
    ) -> Result<Self, ServiceVolumeGraphError> {
        Ok(Self {
            volumes: self.volumes.scope_to_project(project)?,
            configs: self.configs,
        })
    }

    pub(crate) fn into_parts(self) -> (ServiceVolumeGraph, ServiceConfigGraph) {
        (self.volumes, self.configs)
    }
}

/// Admitted mounts whose managed Volume sources also have Project scope.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedServiceMountGraph {
    volumes: ResolvedServiceVolumeGraph,
    configs: ServiceConfigGraph,
}

impl TryFrom<ServiceMountGraph> for ResolvedServiceMountGraph {
    type Error = ServiceVolumeGraphError;
    fn try_from(graph: ServiceMountGraph) -> Result<Self, Self::Error> {
        Ok(Self {
            volumes: graph.volumes.try_into()?,
            configs: graph.configs,
        })
    }
}

impl ResolvedServiceMountGraph {
    #[must_use]
    pub fn volume_graph(&self) -> &ResolvedServiceVolumeGraph {
        &self.volumes
    }
    #[must_use]
    pub fn config_graph(&self) -> &ServiceConfigGraph {
        &self.configs
    }
    #[must_use]
    pub fn to_requested(&self) -> ServiceMountGraph {
        ServiceMountGraph {
            volumes: self.volumes.to_requested(),
            configs: self.configs.clone(),
        }
    }
    pub(crate) fn into_parts(self) -> (ResolvedServiceVolumeGraph, ServiceConfigGraph) {
        (self.volumes, self.configs)
    }
}

// Linux destination keys are lexical: no filesystem or symlink resolution.
fn admit_target(
    raw: &str,
    destinations: &mut BTreeSet<crate::ContainerPath>,
) -> Result<crate::ContainerPath, ServiceSpecGraphError> {
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(ServiceSpecGraphError::RootMountTarget);
    }
    let target = crate::ContainerPath::parse(format!("/{}", parts.join("/")))
        .expect("canonical target is absolute");
    if !destinations.insert(target.clone()) {
        return Err(ServiceSpecGraphError::DuplicateMountTarget { target });
    }
    Ok(target)
}
