use std::collections::BTreeMap;

use ployz_core::ids::{
    NamespaceRevisionEntryId, OperationId, ServiceId, StepId, SubjectTokenError,
};
use ployz_core::machine_runtime::ManagedContainerKind;
use ployz_core::ops::{RoutePort, RoutePortError};
use serde::{Deserialize, Serialize};

pub const MANAGED_LABEL: &str = "plz.managed";
pub const SERVICE_ID_LABEL: &str = "plz.service_id";
pub const REVISION_LABEL: &str = "plz.revision";
pub const OPERATION_ID_LABEL: &str = "plz.operation_id";
pub const STEP_ID_LABEL: &str = "plz.step_id";
pub const CONTAINER_TYPE_LABEL: &str = "plz.container_type";
pub const ENDPOINT_PORT_LABEL: &str = "plz.endpoint_port";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerLabels {
    pub service_id: ServiceId,
    pub revision_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_port: Option<RoutePort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedContainerIdentity {
    pub service_id: ServiceId,
    pub revision_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub kind: ManagedContainerKind,
}

impl ManagedContainerLabels {
    #[must_use]
    pub fn identity(&self) -> ManagedContainerIdentity {
        ManagedContainerIdentity {
            service_id: self.service_id.clone(),
            revision_id: self.revision_id.clone(),
            operation_id: self.operation_id.clone(),
            step_id: self.step_id.clone(),
            kind: self.kind,
        }
    }

    #[must_use]
    pub fn render(&self) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::from([
            (MANAGED_LABEL.to_owned(), "true".to_owned()),
            (
                SERVICE_ID_LABEL.to_owned(),
                self.service_id.as_str().to_owned(),
            ),
            (
                REVISION_LABEL.to_owned(),
                self.revision_id.as_str().to_owned(),
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
        ]);

        if let Some(port) = self.endpoint_port {
            labels.insert(ENDPOINT_PORT_LABEL.to_owned(), port.get().to_string());
        }

        labels
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

        let service_id = parse_id(labels, SERVICE_ID_LABEL, ServiceId::try_new)?;
        let revision_id = parse_id(labels, REVISION_LABEL, NamespaceRevisionEntryId::try_new)?;
        let operation_id = parse_id(labels, OPERATION_ID_LABEL, OperationId::try_new)?;
        let step_id = parse_id(labels, STEP_ID_LABEL, StepId::try_new)?;
        let kind_value = required_label(labels, CONTAINER_TYPE_LABEL)?;
        let Some(kind) = ManagedContainerKind::from_label(kind_value) else {
            return Err(ManagedContainerLabelError::InvalidKind {
                value: kind_value.to_owned(),
            });
        };
        let endpoint_port = match labels.get(ENDPOINT_PORT_LABEL) {
            Some(value) => Some(
                value
                    .parse::<u16>()
                    .map_err(|_| ManagedContainerLabelError::InvalidPort {
                        value: value.clone(),
                    })
                    .and_then(|value| {
                        RoutePort::try_new(value).map_err(|source| {
                            ManagedContainerLabelError::InvalidRoutePort { value, source }
                        })
                    })?,
            ),
            None => None,
        };

        Ok(Self {
            service_id,
            revision_id,
            operation_id,
            step_id,
            kind,
            endpoint_port,
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
    InvalidPort {
        value: String,
    },
    InvalidRoutePort {
        value: u16,
        source: RoutePortError,
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
