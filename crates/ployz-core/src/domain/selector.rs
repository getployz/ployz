//! Typed selectors for Machine, Service, and Container targeting.

use thiserror::Error;

use crate::{
    ContainerId, ContainerObservation, ContainerSelector, Machine, MachineTarget, NameMatches,
    ProjectName, QualifiedService, ServiceId, ServiceName, ServiceObservation, ServiceSelector,
    ServiceSelectorError, ValueError, select_service,
};

impl MachineTarget {
    /// Resolve an exact Machine ID, then an observer-relative Machine Name.
    #[must_use]
    pub fn resolve<'a>(
        &self,
        visible: impl IntoIterator<Item = &'a Machine>,
    ) -> NameMatches<&'a Machine> {
        super::machine::resolve_machine_text(self.as_str(), visible)
    }
}

impl ServiceSelector {
    /// Resolve an exact Service ID, then a Qualified Service, then a unique Service Name.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceSelectorError::NotFound`] when no Service matches, or
    /// [`ServiceSelectorError::NameAmbiguity`] when a short name matches more than one
    /// Qualified Service.
    pub fn resolve<'a>(
        &self,
        services: &'a [ServiceObservation],
    ) -> Result<&'a ServiceObservation, ServiceSelectorError> {
        select_service(services, self)
    }

    /// Qualify a Service Name with `project`. Service IDs and Qualified Services are unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the leftover selector is not a Service Name.
    pub fn with_project(self, project: &ProjectName) -> Result<Self, ValueError> {
        if ServiceId::parse(self.as_str()).is_ok() || QualifiedService::parse(self.as_str()).is_ok()
        {
            return Ok(self);
        }
        let name = ServiceName::parse(self.as_str())?;
        Ok(Self::from(&QualifiedService::new(project.clone(), name)))
    }
}

impl ContainerSelector {
    /// Resolve an exact Container ID, then a display name, then a unique ID prefix.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerSelectorError::NotFound`] when no Container matches, or
    /// [`ContainerSelectorError::Ambiguous`] when a name or prefix matches more than one Container.
    pub fn resolve<'a, T: AsRef<ContainerObservation>>(
        &self,
        containers: impl IntoIterator<Item = &'a T>,
    ) -> Result<&'a T, ContainerSelectorError> {
        resolve_container_selector(containers, self)
    }
}

/// A rejected Container selector lookup.
#[derive(Clone, Debug, Eq, Error, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "error")]
pub enum ContainerSelectorError {
    #[error("Container \"{selector}\" was not found")]
    NotFound { selector: ContainerSelector },
    #[error("Container \"{selector}\" matches multiple containers: {container_ids:?}")]
    Ambiguous {
        selector: ContainerSelector,
        container_ids: Vec<ContainerId>,
    },
}

/// Resolve a Container by exact ID, display name, then unique ID prefix.
///
/// # Errors
///
/// Returns [`ContainerSelectorError::NotFound`] when no Container matches, or
/// [`ContainerSelectorError::Ambiguous`] when a name or prefix matches more than one Container.
pub fn resolve_container_selector<'a, T: AsRef<ContainerObservation>>(
    containers: impl IntoIterator<Item = &'a T>,
    selector: &ContainerSelector,
) -> Result<&'a T, ContainerSelectorError> {
    let containers = containers.into_iter().collect::<Vec<_>>();
    if let Some(container) = containers
        .iter()
        .copied()
        .find(|container| container.as_ref().container_id.as_str() == selector.as_str())
    {
        return Ok(container);
    }
    let named = containers
        .iter()
        .copied()
        .filter(|container| container.as_ref().display_name == selector.as_str())
        .collect::<Vec<_>>();
    match named.as_slice() {
        [container] => return Ok(container),
        [] => {}
        _ => return Err(ambiguous(selector, named)),
    }
    let prefixed = containers
        .iter()
        .copied()
        .filter(|container| {
            container
                .as_ref()
                .container_id
                .as_str()
                .starts_with(selector.as_str())
        })
        .collect::<Vec<_>>();
    match prefixed.as_slice() {
        [] => Err(ContainerSelectorError::NotFound {
            selector: selector.clone(),
        }),
        [container] => Ok(*container),
        _ => Err(ambiguous(selector, prefixed)),
    }
}

fn ambiguous<T: AsRef<ContainerObservation>>(
    selector: &ContainerSelector,
    matches: Vec<&T>,
) -> ContainerSelectorError {
    ContainerSelectorError::Ambiguous {
        selector: selector.clone(),
        container_ids: matches
            .into_iter()
            .map(|container| container.as_ref().container_id)
            .collect(),
    }
}
