use std::{collections::BTreeMap, path::PathBuf};

use ployz_core::{RequestedServiceSpec, ServiceDependency};
use serde::{Deserialize, Serialize};
use serde_norway::Value;
use thiserror::Error;

/// A Compose build held as the raw spec. Additional contexts are read from `raw`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BuildSpec {
    pub raw: Value,
}

/// A project secret after validate: one source, or the resolved value.
#[derive(Clone, Debug, Eq, Serialize, Deserialize, PartialEq)]
pub(crate) enum ProjectSecret {
    Unresolved(SecretSource),
    Resolved(String),
}

/// How an unresolved project secret is obtained.
#[derive(Clone, Debug, Eq, Serialize, Deserialize, PartialEq)]
pub(crate) enum SecretSource {
    File(String),
    Environment(String),
    Command(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ComposeProject {
    pub name: String,
    pub working_dir: PathBuf,
    pub context: Option<String>,
    #[serde(deserialize_with = "deserialize_services")]
    pub services: BTreeMap<String, RequestedServiceSpec>,
    pub builds: BTreeMap<String, BuildSpec>,
    pub dependencies: BTreeMap<String, Vec<ServiceDependency>>,
    pub warnings: Vec<String>,
    pub service_profiles: BTreeMap<String, Vec<String>>,
    pub(super) secrets: BTreeMap<String, ProjectSecret>,
    pub(super) environment: BTreeMap<String, String>,
}

impl ComposeProject {
    #[must_use]
    pub fn selected_context<'a>(
        &'a self,
        explicit_context: Option<&'a str>,
        explicit_connect: Option<&str>,
    ) -> Option<&'a str> {
        explicit_context.or_else(|| {
            explicit_connect
                .is_none()
                .then_some(self.context.as_deref())
                .flatten()
        })
    }

    pub fn select_services(&self, selected: &[String]) -> Result<Self, ComposeError> {
        if selected.is_empty() {
            return Ok(self.clone());
        }
        let mut included = std::collections::BTreeSet::new();
        let mut pending = selected.to_vec();
        while let Some(name) = pending.pop() {
            if !self.services.contains_key(&name) {
                return Err(ComposeError::Invalid(format!("undefined service '{name}'")));
            }
            if included.insert(name.clone()) {
                pending.extend(
                    self.dependencies
                        .get(&name)
                        .into_iter()
                        .flatten()
                        .map(|dependency| dependency.service.to_string()),
                );
            }
        }
        let mut project = self.clone();
        project.services.retain(|name, _| included.contains(name));
        project.builds.retain(|name, _| included.contains(name));
        project
            .service_profiles
            .retain(|name, _| included.contains(name));
        project.dependencies.retain(|name, dependencies| {
            if !included.contains(name) {
                return false;
            }
            dependencies.retain(|dependency| included.contains(dependency.service.as_str()));
            true
        });
        Ok(project)
    }

    /// Compose `profiles:` per loaded Service. Empty means the Service always starts.
    #[must_use]
    pub fn service_profiles(&self) -> BTreeMap<ployz_core::ServiceName, Vec<String>> {
        self.service_profiles
            .iter()
            .filter_map(|(name, profiles)| {
                Some((ployz_core::ServiceName::parse(name).ok()?, profiles.clone()))
            })
            .collect()
    }

    /// Service keys that start for `requested_profiles`. Unprofiled Services always start.
    #[must_use]
    pub fn enabled_service_names(&self, requested_profiles: &[String]) -> Vec<String> {
        self.services
            .keys()
            .filter(|name| {
                ployz_core::profiles_enable_start(
                    self.service_profiles
                        .get(*name)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    requested_profiles,
                )
            })
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ComposeError {
    #[error("{0}")]
    Prerequisite(String),
    #[error("{0}")]
    Compose(String),
    #[error("invalid normalized Compose project: {0}")]
    Invalid(String),
    #[error("building {services}: {source}")]
    Build {
        services: String,
        #[source]
        source: ployz_build::BuildError,
    },
    #[error("{0}")]
    Io(String),
}

fn deserialize_services<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, RequestedServiceSpec>, D::Error> {
    BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?
        .into_iter()
        .map(|(name, value)| {
            serde_json::from_value(value)
                .map_err(|error| serde::de::Error::custom(format!("service '{name}': {error}")))
                .map(|spec| (name, spec))
        })
        .collect()
}
