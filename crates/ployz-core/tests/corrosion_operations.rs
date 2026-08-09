use ployz_core::corrosion::{
    CorrosionDeployOutcome, CorrosionDeployState, CorrosionDocumentVersion, CorrosionTimestamp,
    OperationDocument, Principal,
};
use ployz_core::ids::{ClusterName, CorrosionNamespaceName, DeployName, MachineName, PeerName};

fn timestamp(value: &str) -> CorrosionTimestamp {
    CorrosionTimestamp::try_new(value).expect("timestamp")
}

fn deploy() -> OperationDocument {
    OperationDocument::deploy_created(
        CorrosionDocumentVersion::V1,
        ClusterName::try_new("main").expect("cluster"),
        MachineName::try_new("edge-a").expect("machine"),
        Principal::Peer {
            peer_id: PeerName::try_new("operator").expect("peer"),
        },
        CorrosionNamespaceName::try_new("production").expect("namespace"),
        DeployName::try_new("deploy-a").expect("deploy"),
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
