//! Docker label codec for Corrosion-owned v2 container identities.

use std::collections::BTreeMap;

use ployz_core::corrosion::V2ManagedContainerIdentity;
use ployz_core::ids::{CorrosionUlidError, NamespaceRowId, OperationRowId, ServiceRowId};

use super::labels::MANAGED_LABEL;

pub const IDENTITY_SCHEMA_LABEL: &str = "plz.identity_schema";
pub const V2_IDENTITY_SCHEMA: &str = "corrosion_v2";
pub const NAMESPACE_ROW_ID_LABEL: &str = "plz.namespace_row_id";
pub const SERVICE_ROW_ID_LABEL: &str = "plz.service_row_id";
pub const OPERATION_ROW_ID_LABEL: &str = "plz.operation_row_id";

#[must_use]
pub fn render(identity: &V2ManagedContainerIdentity) -> BTreeMap<String, String> {
    BTreeMap::from([
        (MANAGED_LABEL.to_owned(), "true".to_owned()),
        (
            IDENTITY_SCHEMA_LABEL.to_owned(),
            V2_IDENTITY_SCHEMA.to_owned(),
        ),
        (
            NAMESPACE_ROW_ID_LABEL.to_owned(),
            identity.namespace_id.as_str().to_owned(),
        ),
        (
            SERVICE_ROW_ID_LABEL.to_owned(),
            identity.service_id.as_str().to_owned(),
        ),
        (
            OPERATION_ROW_ID_LABEL.to_owned(),
            identity.operation_id.as_str().to_owned(),
        ),
    ])
}

pub fn parse(
    labels: &BTreeMap<String, String>,
) -> Result<V2ManagedContainerIdentity, V2ManagedContainerLabelError> {
    require_exact(labels, MANAGED_LABEL, "true")?;
    require_exact(labels, IDENTITY_SCHEMA_LABEL, V2_IDENTITY_SCHEMA)?;

    Ok(V2ManagedContainerIdentity {
        namespace_id: parse_row_id(labels, NAMESPACE_ROW_ID_LABEL, NamespaceRowId::try_new)?,
        service_id: parse_row_id(labels, SERVICE_ROW_ID_LABEL, ServiceRowId::try_new)?,
        operation_id: parse_row_id(labels, OPERATION_ROW_ID_LABEL, OperationRowId::try_new)?,
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
    InvalidRowId {
        label: &'static str,
        source: CorrosionUlidError,
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

fn parse_row_id<Id>(
    labels: &BTreeMap<String, String>,
    label: &'static str,
    parse: impl FnOnce(String) -> Result<Id, CorrosionUlidError>,
) -> Result<Id, V2ManagedContainerLabelError> {
    parse(required_label(labels, label)?.to_owned())
        .map_err(|source| V2ManagedContainerLabelError::InvalidRowId { label, source })
}

#[cfg(test)]
#[path = "v2_labels_tests.rs"]
mod tests;
