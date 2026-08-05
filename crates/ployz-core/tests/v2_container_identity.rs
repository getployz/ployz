use ployz_core::corrosion::V2ManagedContainerIdentity;
use ployz_core::ids::{NamespaceRowId, OperationRowId, ServiceRowId};
use serde_json::json;

const NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const OPERATION: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

fn identity() -> V2ManagedContainerIdentity {
    V2ManagedContainerIdentity {
        namespace_id: NamespaceRowId::try_new(NAMESPACE).expect("namespace row id"),
        service_id: ServiceRowId::try_new(SERVICE).expect("service row id"),
        operation_id: OperationRowId::try_new(OPERATION).expect("operation row id"),
    }
}

#[test]
fn v2_identity_has_an_exact_row_id_only_wire_shape() {
    assert_eq!(
        serde_json::to_value(identity()).expect("identity serializes"),
        json!({
            "namespace_id": NAMESPACE,
            "service_id": SERVICE,
            "operation_id": OPERATION,
        })
    );
}

#[test]
fn v2_identity_rejects_incumbent_revision_and_step_fields() {
    let incumbent_shape = json!({
        "namespace_id": NAMESPACE,
        "service_id": SERVICE,
        "operation_id": OPERATION,
        "namespace_revision_entry_id": "revision",
        "step_id": "step",
        "kind": "service",
    });

    assert!(serde_json::from_value::<V2ManagedContainerIdentity>(incumbent_shape).is_err());
}
