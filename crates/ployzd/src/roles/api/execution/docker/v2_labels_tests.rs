use std::collections::BTreeMap;

use ployz_core::corrosion::V2ManagedContainerIdentity;
use ployz_core::ids::{NamespaceRowId, OperationRowId, ServiceRowId};

use super::{
    IDENTITY_SCHEMA_LABEL, MANAGED_LABEL, NAMESPACE_ROW_ID_LABEL, OPERATION_ROW_ID_LABEL,
    SERVICE_ROW_ID_LABEL, V2_IDENTITY_SCHEMA, V2ManagedContainerLabelError, parse, render,
};

#[test]
fn v2_identity_round_trips_without_incumbent_revision_or_step_labels() {
    let identity = v2_identity();
    let labels = render(&identity);

    assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
    assert_eq!(
        labels.get(IDENTITY_SCHEMA_LABEL).map(String::as_str),
        Some(V2_IDENTITY_SCHEMA)
    );
    assert_eq!(labels.len(), 5);
    assert_eq!(parse(&labels), Ok(identity));
}

#[test]
fn v2_identity_rejects_an_invalid_row_id() {
    let mut labels = render(&v2_identity());
    labels.insert(SERVICE_ROW_ID_LABEL.to_owned(), "svc_api".to_owned());

    assert!(matches!(
        parse(&labels),
        Err(V2ManagedContainerLabelError::InvalidRowId {
            label: SERVICE_ROW_ID_LABEL,
            ..
        })
    ));
}

#[test]
fn v2_identity_requires_its_schema_discriminator() {
    let identity = v2_identity();
    let labels = BTreeMap::from([
        (MANAGED_LABEL.to_owned(), "true".to_owned()),
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
    ]);

    assert_eq!(
        parse(&labels),
        Err(V2ManagedContainerLabelError::Missing {
            label: IDENTITY_SCHEMA_LABEL
        })
    );
}

fn v2_identity() -> V2ManagedContainerIdentity {
    V2ManagedContainerIdentity {
        namespace_id: NamespaceRowId::try_new("01K00000000000000000000001").expect("namespace"),
        service_id: ServiceRowId::try_new("01K00000000000000000000002").expect("service"),
        operation_id: OperationRowId::try_new("01K00000000000000000000003").expect("operation"),
    }
}
