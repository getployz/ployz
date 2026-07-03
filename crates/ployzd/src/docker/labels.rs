use std::collections::BTreeMap;

use ployz_core::ids::{
    NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId, SubjectTokenError,
};
use ployz_core::machine_runtime::ManagedContainerKind;
use serde::{Deserialize, Serialize};

pub const MANAGED_LABEL: &str = "plz.managed";
pub const NAMESPACE_ID_LABEL: &str = "plz.namespace_id";
pub const SERVICE_ID_LABEL: &str = "plz.service_id";
pub const NAMESPACE_REVISION_ENTRY_LABEL: &str = "plz.namespace_revision_entry";
pub const OPERATION_ID_LABEL: &str = "plz.operation_id";
pub const STEP_ID_LABEL: &str = "plz.step_id";
pub const CONTAINER_TYPE_LABEL: &str = "plz.container_type";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerLabels {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerIdentity {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
}

impl ManagedContainerLabels {
    #[must_use]
    pub fn identity(&self) -> ManagedContainerIdentity {
        ManagedContainerIdentity {
            namespace_id: self.namespace_id.clone(),
            service_id: self.service_id.clone(),
            namespace_revision_entry_id: self.namespace_revision_entry_id.clone(),
            operation_id: self.operation_id.clone(),
            step_id: self.step_id.clone(),
            kind: self.kind,
        }
    }

    #[must_use]
    pub fn render(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (MANAGED_LABEL.to_owned(), "true".to_owned()),
            (
                NAMESPACE_ID_LABEL.to_owned(),
                self.namespace_id.as_str().to_owned(),
            ),
            (
                SERVICE_ID_LABEL.to_owned(),
                self.service_id.as_str().to_owned(),
            ),
            (
                NAMESPACE_REVISION_ENTRY_LABEL.to_owned(),
                self.namespace_revision_entry_id.as_str().to_owned(),
            ),
            (
                OPERATION_ID_LABEL.to_owned(),
                self.operation_id.as_str().to_owned(),
            ),
            (STEP_ID_LABEL.to_owned(), self.step_id.as_str().to_owned()),
            (
                CONTAINER_TYPE_LABEL.to_owned(),
                self.kind.as_label().to_owned(),
            ),
        ])
    }

    pub fn parse(labels: &BTreeMap<String, String>) -> Result<Self, ManagedContainerLabelError> {
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
        let namespace_revision_entry_id = parse_id(labels, NAMESPACE_REVISION_ENTRY_LABEL, NamespaceRevisionEntryId::try_new)?;
        let operation_id = parse_id(labels, OPERATION_ID_LABEL, OperationId::try_new)?;
        let step_id = parse_id(labels, STEP_ID_LABEL, StepId::try_new)?;
        let kind_value = required_label(labels, CONTAINER_TYPE_LABEL)?;
        let Some(kind) = ManagedContainerKind::from_label(kind_value) else {
            return Err(ManagedContainerLabelError::InvalidKind {
                value: kind_value.to_owned(),
            });
        };
        Ok(Self {
            namespace_id,
            service_id,
            namespace_revision_entry_id,
            operation_id,
            step_id,
            kind,
        })
    }
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
