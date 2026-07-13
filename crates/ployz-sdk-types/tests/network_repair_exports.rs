use ployz_sdk_types::{DataplaneProjection, OperationEvent};

#[test]
fn sdk_exports_network_repair_dataplane_converged_event() {
    let revision = DataplaneProjection::try_new(Vec::new(), None)
        .expect("empty projection")
        .declared_revision()
        .clone();
    let event = OperationEvent::NetworkRepairDataplaneConverged {
        operation_id: ployz_sdk_types::OperationId::try_new("op_network_repair")
            .expect("valid operation id"),
        revision: revision.clone(),
        machine_ids: vec![
            ployz_sdk_types::MachineId::try_new("machine_a").expect("valid machine id"),
        ],
    };

    let json = serde_json::to_value(event).expect("event serializes");
    assert_eq!(
        json.get("event"),
        Some(&serde_json::json!("network_repair_dataplane_converged"))
    );
    assert_eq!(
        json.get("revision"),
        Some(&serde_json::json!(revision.as_str()))
    );
    assert_eq!(
        json.get("machine_ids"),
        Some(&serde_json::json!(["machine_a"]))
    );
}
