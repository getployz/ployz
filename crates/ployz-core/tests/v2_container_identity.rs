use ployz_core::corrosion::{CorrosionServiceName, V2ManagedContainerIdentity};
use ployz_core::deploy::ReplicaSlot;
use ployz_core::ids::{CorrosionNamespaceName, DeployName};
use serde_json::json;

const NAMESPACE: &str = "production";
const SERVICE: &str = "api";
const OPERATION: &str = "release-42";

fn identity() -> V2ManagedContainerIdentity {
    V2ManagedContainerIdentity {
        namespace_id: CorrosionNamespaceName::try_new(NAMESPACE).expect("namespace row id"),
        service_name: CorrosionServiceName::try_new(SERVICE).expect("service name"),
        operation_id: DeployName::try_new(OPERATION).expect("operation row id"),
        replica_slot: ReplicaSlot::Global,
    }
}

#[test]
fn v2_identity_has_an_exact_row_and_replica_slot_wire_shape() {
    assert_eq!(
        serde_json::to_value(identity()).expect("identity serializes"),
        json!({
            "namespace_id": NAMESPACE,
            "service_name": SERVICE,
            "operation_id": OPERATION,
            "replica_slot": { "kind": "global" },
        })
    );
}

#[test]
fn v2_identity_rejects_incumbent_revision_and_step_fields() {
    let incumbent_shape = json!({
        "namespace_id": NAMESPACE,
        "service_name": SERVICE,
        "operation_id": OPERATION,
        "namespace_revision_entry_id": "revision",
        "step_id": "step",
        "kind": "service",
    });

    assert!(serde_json::from_value::<V2ManagedContainerIdentity>(incumbent_shape).is_err());
}
