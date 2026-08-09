//! Docker label codec for Corrosion-owned v2 container identities.

use std::collections::BTreeMap;

use ployz_core::corrosion::{CorrosionServiceName, V2ManagedContainerIdentity};
use ployz_core::deploy::{ReplicaSlot, ReplicatedReplicaSlot};
use ployz_core::ids::{CorrosionNamespaceName, DeployName};

pub(super) const MANAGED_LABEL: &str = "plz.managed";

pub const IDENTITY_SCHEMA_LABEL: &str = "plz.identity_schema";
pub const V2_IDENTITY_SCHEMA: &str = "corrosion_v2";
pub const NAMESPACE_NAME_LABEL: &str = "plz.namespace";
pub const SERVICE_NAME_LABEL: &str = "plz.service";
pub const DEPLOY_NAME_LABEL: &str = "plz.deploy";
pub const REPLICA_SLOT_LABEL: &str = "plz.replica_slot";

#[must_use]
pub fn render(identity: &V2ManagedContainerIdentity) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_LABEL.to_owned(), "true".to_owned()),
        (
            IDENTITY_SCHEMA_LABEL.to_owned(),
            V2_IDENTITY_SCHEMA.to_owned(),
        ),
        (
            NAMESPACE_NAME_LABEL.to_owned(),
            identity.namespace_id.as_str().to_owned(),
        ),
        (
            SERVICE_NAME_LABEL.to_owned(),
            identity.service_name.as_str().to_owned(),
        ),
        (
            DEPLOY_NAME_LABEL.to_owned(),
            identity.operation_id.as_str().to_owned(),
        ),
        (
            REPLICA_SLOT_LABEL.to_owned(),
            match identity.replica_slot {
                ReplicaSlot::Global => "global".to_owned(),
                ReplicaSlot::Replicated { number } => number.get().to_string(),
            },
        ),
    ])
}

pub fn parse(
    labels: &BTreeMap<String, String>,
) -> Result<V2ManagedContainerIdentity, V2ManagedContainerLabelError> {
    require_exact(labels, MANAGED_LABEL, "true")?;
    require_exact(labels, IDENTITY_SCHEMA_LABEL, V2_IDENTITY_SCHEMA)?;

    Ok(V2ManagedContainerIdentity {
        namespace_id: parse_name(
            labels,
            NAMESPACE_NAME_LABEL,
            CorrosionNamespaceName::try_new,
        )?,
        service_name: parse_name(labels, SERVICE_NAME_LABEL, CorrosionServiceName::try_new)?,
        operation_id: parse_name(labels, DEPLOY_NAME_LABEL, DeployName::try_new)?,
        replica_slot: parse_replica_slot(labels)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2ManagedContainerLabelError {
    Missing {
        label: &'static str,
    },
    UnexpectedValue {
        label: &'static str,
        expected: &'static str,
        value: String,
    },
    InvalidName {
        label: &'static str,
        source: String,
    },
    InvalidReplicaSlot {
        value: String,
    },
}

fn required_label<'a>(
    labels: &'a BTreeMap<String, String>,
    label: &'static str,
) -> Result<&'a str, V2ManagedContainerLabelError> {
    labels
        .get(label)
        .map(String::as_str)
        .ok_or(V2ManagedContainerLabelError::Missing { label })
}

fn require_exact(
    labels: &BTreeMap<String, String>,
    label: &'static str,
    expected: &'static str,
) -> Result<(), V2ManagedContainerLabelError> {
    let value = required_label(labels, label)?;
    if value == expected {
        return Ok(());
    }
    Err(V2ManagedContainerLabelError::UnexpectedValue {
        label,
        expected,
        value: value.to_owned(),
    })
}

fn parse_name<Name, Error>(
    labels: &BTreeMap<String, String>,
    label: &'static str,
    parse: impl FnOnce(String) -> Result<Name, Error>,
) -> Result<Name, V2ManagedContainerLabelError>
where
    Error: std::fmt::Display,
{
    parse(required_label(labels, label)?.to_owned()).map_err(|source| {
        V2ManagedContainerLabelError::InvalidName {
            label,
            source: source.to_string(),
        }
    })
}

fn parse_replica_slot(
    labels: &BTreeMap<String, String>,
) -> Result<ReplicaSlot, V2ManagedContainerLabelError> {
    let value = required_label(labels, REPLICA_SLOT_LABEL)?;
    if value == "global" {
        return Ok(ReplicaSlot::Global);
    }
    value
        .parse::<u16>()
        .ok()
        .and_then(|number| ReplicatedReplicaSlot::try_new(number).ok())
        .map(|number| ReplicaSlot::Replicated { number })
        .ok_or_else(|| V2ManagedContainerLabelError::InvalidReplicaSlot {
            value: value.to_owned(),
        })
}

#[cfg(test)]
#[path = "v2_labels_tests.rs"]
mod tests;
