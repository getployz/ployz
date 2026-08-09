use ployz_core::corrosion::{
    CorrosionDeployOutcome, CorrosionDeployState, CorrosionDocumentVersion, CorrosionTimestamp,
    OperationDocument, Principal,
};
use ployz_core::ids::{ClusterId, MachineRowId, NamespaceRowId, PeerId, ServiceRowId};

fn timestamp(value: &str) -> CorrosionTimestamp {
    CorrosionTimestamp::try_new(value).expect("timestamp")
}

fn deploy() -> OperationDocument {
    OperationDocument::deploy_created(
        CorrosionDocumentVersion::V1,
        ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAB").expect("cluster"),
        MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAC").expect("machine"),
        Principal::Peer {
            peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAD").expect("peer"),
        },
        NamespaceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAF").expect("namespace"),
        ServiceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAE").expect("service"),
        timestamp("2026-08-08T12:00:00Z"),
    )
}

#[test]
fn deploy_is_one_created_snapshot_followed_by_one_terminal_snapshot() {
    let created = deploy();
    assert_eq!(created.deploy_state(), &CorrosionDeployState::Created);

    let terminal = created.into_terminal(
        timestamp("2026-08-08T12:00:02Z"),
        CorrosionDeployOutcome::Interrupted,
    );
    assert!(terminal.is_terminal());

    let encoded = serde_json::to_value(&terminal).expect("operation JSON");
    assert_eq!(
        encoded.get("state").and_then(serde_json::Value::as_str),
        Some("terminal")
    );
    let outcome = encoded
        .get("outcome")
        .and_then(serde_json::Value::as_object)
        .expect("terminal outcome");
    assert_eq!(
        outcome.get("kind").and_then(serde_json::Value::as_str),
        Some("interrupted")
    );
    assert!(encoded.get("kind").is_none());
    assert!(encoded.get("appointment_id").is_none());
    assert!(!outcome.contains_key("service_id"));
    assert!(!outcome.contains_key("resubmit"));
    assert_eq!(
        serde_json::from_value::<OperationDocument>(encoded).expect("round-trip"),
        terminal
    );
}
