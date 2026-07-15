//! Docker label codec for [`ManagedContainerIdentity`].
//!
//! Labels are recovery evidence: the identity is rendered into flat `plz.*`
//! string pairs at container creation and parsed back from Docker summaries.
//! The identity struct itself lives in `ployz-core`; this module owns only
//! the label wire format.
//!
//! Changing a label name or required label intentionally breaks existing
//! managed containers unless paired with container cleanup or a label migration.

use std::collections::BTreeMap;

use ployz_core::ids::{
    NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId, SubjectTokenError,
};
use ployz_core::machine_runtime::{ManagedContainerIdentity, ManagedContainerKind};

pub const MANAGED_LABEL: &str = "plz.managed";
pub const NAMESPACE_ID_LABEL: &str = "plz.namespace_id";
pub const SERVICE_ID_LABEL: &str = "plz.service_id";
pub const NAMESPACE_REVISION_ENTRY_LABEL: &str = "plz.namespace_revision_entry";
pub const OPERATION_ID_LABEL: &str = "plz.operation_id";
pub const STEP_ID_LABEL: &str = "plz.step_id";
pub const CONTAINER_TYPE_LABEL: &str = "plz.container_type";

#[must_use]
pub fn render(identity: &ManagedContainerIdentity) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_LABEL.to_owned(), "true".to_owned()),
        (
            NAMESPACE_ID_LABEL.to_owned(),
            identity.namespace_id.as_str().to_owned(),
        ),
        (
            SERVICE_ID_LABEL.to_owned(),
            identity.service_id.as_str().to_owned(),
        ),
        (
            NAMESPACE_REVISION_ENTRY_LABEL.to_owned(),
            identity.namespace_revision_entry_id.as_str().to_owned(),
        ),
        (
            OPERATION_ID_LABEL.to_owned(),
            identity.operation_id.as_str().to_owned(),
        ),
        (
            STEP_ID_LABEL.to_owned(),
            identity.step_id.as_str().to_owned(),
        ),
        (
            CONTAINER_TYPE_LABEL.to_owned(),
            identity.kind.as_label().to_owned(),
        ),
    ])
}

pub fn parse(
    labels: &BTreeMap<String, String>,
) -> Result<ManagedContainerIdentity, ManagedContainerLabelError> {
    match required_label(labels, MANAGED_LABEL)? {
        "true" => {}
        value => {
            return Err(ManagedContainerLabelError::InvalidManagedValue {
                value: value.to_owned(),
            });
        }
    }

    let namespace_id = parse_id(labels, NAMESPACE_ID_LABEL, NamespaceId::try_new)?;
    let service_id = parse_id(labels, SERVICE_ID_LABEL, ServiceId::try_new)?;
    let namespace_revision_entry_id = parse_id(
        labels,
        NAMESPACE_REVISION_ENTRY_LABEL,
        NamespaceRevisionEntryId::try_new,
    )?;
    let operation_id = parse_id(labels, OPERATION_ID_LABEL, OperationId::try_new)?;
    let step_id = parse_id(labels, STEP_ID_LABEL, StepId::try_new)?;
    let kind_value = required_label(labels, CONTAINER_TYPE_LABEL)?;
    let Some(kind) = ManagedContainerKind::from_label(kind_value) else {
        return Err(ManagedContainerLabelError::InvalidKind {
            value: kind_value.to_owned(),
        });
    };
    Ok(ManagedContainerIdentity {
        namespace_id,
        service_id,
        namespace_revision_entry_id,
        operation_id,
        step_id,
        kind,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedContainerLabelError {
    Missing {
        label: &'static str,
    },
    InvalidManagedValue {
        value: String,
    },
    InvalidKind {
        value: String,
    },
    InvalidId {
        label: &'static str,
        source: SubjectTokenError,
    },
}

fn required_label<'a>(
    labels: &'a BTreeMap<String, String>,
    label: &'static str,
) -> Result<&'a str, ManagedContainerLabelError> {
    labels
        .get(label)
        .map(String::as_str)
        .ok_or(ManagedContainerLabelError::Missing { label })
}

fn parse_id<Id>(
    labels: &BTreeMap<String, String>,
    label: &'static str,
    parse: impl FnOnce(String) -> Result<Id, SubjectTokenError>,
) -> Result<Id, ManagedContainerLabelError> {
    parse(required_label(labels, label)?.to_owned())
        .map_err(|source| ManagedContainerLabelError::InvalidId { label, source })
}

#[cfg(test)]
#[path = "labels_tests.rs"]
mod tests;
