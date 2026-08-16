//! Typed selectors for Machine, Service, and Container targeting.

use thiserror::Error;

use crate::{
    ContainerId, ContainerObservation, ContainerSelector, Machine, MachineTarget, NameMatches,
    ServiceObservation, ServiceSelector, ServiceSelectorError, select_service,
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
    /// Resolve an exact Service ID, then a Service Name, reporting every ambiguous match.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceSelectorError::NotFound`] when no Service matches, or
    /// [`ServiceSelectorError::NameAmbiguity`] when a name matches more than one Service ID.
    pub fn resolve<'a>(
        &self,
        services: &'a [ServiceObservation],
    ) -> Result<&'a ServiceObservation, ServiceSelectorError> {
        select_service(services, self.as_str())
    }
}

impl ContainerSelector {
    /// Resolve an exact Container ID, then a display name, then a unique ID prefix.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerSelectorError::NotFound`] when no Container matches, or
    /// [`ContainerSelectorError::Ambiguous`] when a name or prefix matches more than one Container.
    pub fn resolve<'a>(
        &self,
        containers: impl IntoIterator<Item = &'a ContainerObservation>,
    ) -> Result<&'a ContainerObservation, ContainerSelectorError> {
        resolve_container_selector(containers, self)
    }
}

/// A rejected Container selector lookup.
#[derive(Clone, Debug, Eq, Error, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "error")]
pub enum ContainerSelectorError {
    #[error("Container {selector:?} was not found")]
    NotFound { selector: ContainerSelector },
    #[error("Container {selector:?} matches multiple containers: {container_ids:?}")]
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
pub fn resolve_container_selector<'a>(
    containers: impl IntoIterator<Item = &'a ContainerObservation>,
    selector: &ContainerSelector,
) -> Result<&'a ContainerObservation, ContainerSelectorError> {
    let containers = containers.into_iter().collect::<Vec<_>>();
    if let Some(container) = containers
        .iter()
        .copied()
        .find(|container| container.container_id.as_str() == selector.as_str())
    {
        return Ok(container);
    }
    let named = containers
        .iter()
        .copied()
        .filter(|container| container.display_name == selector.as_str())
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

fn ambiguous(
    selector: &ContainerSelector,
    matches: Vec<&ContainerObservation>,
) -> ContainerSelectorError {
    ContainerSelectorError::Ambiguous {
        selector: selector.clone(),
        container_ids: matches
            .into_iter()
            .map(|container| container.container_id)
            .collect(),
    }
}
