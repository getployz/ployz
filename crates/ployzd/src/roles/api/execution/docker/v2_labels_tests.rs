use std::collections::BTreeMap;

use ployz_core::corrosion::{CorrosionServiceName, V2ManagedContainerIdentity};
use ployz_core::deploy::ReplicaSlot;
use ployz_core::ids::{CorrosionNamespaceName, DeployName};

use super::{
    DEPLOY_NAME_LABEL, IDENTITY_SCHEMA_LABEL, MANAGED_LABEL, NAMESPACE_NAME_LABEL,
    SERVICE_NAME_LABEL, V2_IDENTITY_SCHEMA, V2ManagedContainerLabelError, parse, render,
};

#[test]
fn v2_identity_round_trips_with_its_replica_slot() {
    let identity = v2_identity();
    let labels = render(&identity);

    assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
    assert_eq!(
        labels.get(IDENTITY_SCHEMA_LABEL).map(String::as_str),
        Some(V2_IDENTITY_SCHEMA)
    );
    assert_eq!(labels.len(), 6);
    assert_eq!(parse(&labels), Ok(identity));
}

#[test]
fn v2_identity_rejects_an_invalid_service_name() {
    let mut labels = render(&v2_identity());
    labels.insert(SERVICE_NAME_LABEL.to_owned(), "svc_api".to_owned());

    assert!(matches!(
        parse(&labels),
        Err(V2ManagedContainerLabelError::InvalidName {
            label: SERVICE_NAME_LABEL,
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
        namespace_id: CorrosionNamespaceName::try_new("production").expect("namespace"),
        service_name: CorrosionServiceName::try_new("api").expect("service"),
        operation_id: DeployName::try_new("release-1").expect("operation"),
        replica_slot: ReplicaSlot::Global,
    }
}
