use ployz_core::corrosion::{
    CorrosionBasicOperation, CorrosionBasicTransition, CorrosionDeployCancellationReason,
    CorrosionDeployFailure, CorrosionDeployOutcome, CorrosionDeployOutcomeError,
    CorrosionDeployServiceFailure, CorrosionDeployServiceResult, CorrosionDeployState,
    CorrosionDeployTargets, CorrosionDeployTransition, CorrosionDocumentVersion,
    CorrosionEvidenceLossReason, CorrosionOperation, CorrosionOperationFailure,
    CorrosionOperationState, CorrosionPromotionFailure, CorrosionPromotionRowObservation,
    CorrosionTimestamp, OperationDocument, OperationInitiator,
};
use ployz_core::ids::{
    ClusterId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, ServiceRowId,
};
use serde_json::json;

const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";

fn timestamp(value: &str) -> CorrosionTimestamp {
    CorrosionTimestamp::try_new(value).expect("timestamp")
}

fn service_id() -> ServiceRowId {
    ServiceRowId::try_new(SERVICE).expect("service id")
}

fn created_deploy() -> OperationDocument {
    OperationDocument::deploy_created(
        CorrosionDocumentVersion::V1,
        ClusterId::try_new(CLUSTER).expect("cluster id"),
        MachineRowId::try_new(MACHINE).expect("machine id"),
        OperationInitiator::Peer {
            peer_id: PeerId::try_new(PEER).expect("peer id"),
        },
        NamespaceRowId::try_new(NAMESPACE).expect("namespace id"),
        CorrosionDeployTargets::try_new(vec![service_id()]).expect("deploy targets"),
        timestamp("2026-08-05T10:00:00Z"),
    )
}

fn created_build() -> OperationDocument {
    OperationDocument::basic_created(
        CorrosionDocumentVersion::V1,
        ClusterId::try_new(CLUSTER).expect("cluster id"),
        MachineRowId::try_new(MACHINE).expect("machine id"),
        OperationInitiator::Peer {
            peer_id: PeerId::try_new(PEER).expect("peer id"),
        },
        CorrosionBasicOperation::Build {
            service_id: service_id(),
        },
        timestamp("2026-08-05T10:00:00Z"),
    )
}

#[test]
fn deploy_summary_transitions_created_running_terminal_with_stable_targets() {
    let running = created_deploy()
        .transition_deploy(CorrosionDeployTransition::Running {
            started_at: timestamp("2026-08-05T10:00:01Z"),
        })
        .expect("running transition");
    let completed = running
        .transition_deploy(CorrosionDeployTransition::Terminal {
            completed_at: timestamp("2026-08-05T10:00:02Z"),
            outcome: CorrosionDeployOutcome::completed(vec![
                CorrosionDeployServiceResult::completed(service_id()),
            ])
            .expect("completed outcome"),
        })
        .expect("terminal transition");

    assert!(matches!(
        completed.deploy_state(),
        Some(CorrosionDeployState::Terminal { .. })
    ));
    assert_eq!(
        serde_json::to_value(completed).expect("operation JSON"),
        json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "machine_id": MACHINE,
            "kind": "deploy",
            "namespace_id": NAMESPACE,
            "service_ids": [SERVICE],
            "initiator": { "kind": "peer", "peer_id": PEER },
            "state": "terminal",
            "created_at": "2026-08-05T10:00:00.000000000Z",
            "started_at": "2026-08-05T10:00:01.000000000Z",
            "completed_at": "2026-08-05T10:00:02.000000000Z",
            "outcome": "completed",
            "results": [{ "service_id": SERVICE, "result": "completed" }]
        })
    );
}

#[test]
fn terminal_transition_error_preserves_the_original_document() {
    let terminal = created_deploy()
        .transition_deploy(CorrosionDeployTransition::Terminal {
            completed_at: timestamp("2026-08-05T10:00:02Z"),
            outcome: CorrosionDeployOutcome::completed(vec![
                CorrosionDeployServiceResult::completed(service_id()),
            ])
            .expect("completed outcome"),
        })
        .expect("terminal transition");
    let expected = terminal.clone();

    let error = terminal
        .transition_deploy(CorrosionDeployTransition::Running {
            started_at: timestamp("2026-08-05T10:00:03Z"),
        })
        .expect_err("terminal state is final");

    assert_eq!(error.document(), &expected);
}

#[test]
fn basic_operation_transitions_are_coupled_and_terminal_final() {
    let running = created_build()
        .transition_basic(CorrosionBasicTransition::Running {
            started_at: timestamp("2026-08-05T10:00:01Z"),
        })
        .expect("running transition");
    let failed = running
        .transition_basic(CorrosionBasicTransition::Failed {
            completed_at: timestamp("2026-08-05T10:00:02Z"),
            failure: CorrosionOperationFailure::Interrupted {
                message: "shutdown".to_owned(),
            },
        })
        .expect("terminal transition");

    assert!(matches!(
        failed.operation(),
        CorrosionOperation::Build {
            state: CorrosionOperationState::Failed { .. },
            ..
        }
    ));
    let expected = failed.clone();
    let error = failed
        .transition_basic(CorrosionBasicTransition::Succeeded {
            completed_at: timestamp("2026-08-05T10:00:03Z"),
        })
        .expect_err("terminal state is final");
    assert_eq!(error.document(), &expected);
}

#[test]
fn deploy_outcome_constructors_enforce_the_exhaustive_evidence_matrix() {
    let useful = CorrosionDeployServiceResult::completed(service_id());
    let failed = CorrosionDeployServiceResult::failed(
        ServiceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FB0").expect("service id"),
        CorrosionDeployServiceFailure::ImagePullFailed {
            message: "pull failed".to_owned(),
        },
    );
    let namespace_failure = CorrosionDeployFailure::Interrupted;

    assert_eq!(
        CorrosionDeployOutcome::completed(Vec::new()),
        Err(CorrosionDeployOutcomeError::EmptyResults)
    );
    assert!(CorrosionDeployOutcome::completed(vec![failed.clone()]).is_err());
    assert!(
        CorrosionDeployOutcome::completed_with_warnings(vec![useful.clone()], Vec::new()).is_err()
    );
    assert!(
        CorrosionDeployOutcome::partially_completed(
            vec![useful.clone(), failed.clone()],
            namespace_failure.clone(),
        )
        .is_ok()
    );
    assert!(
        CorrosionDeployOutcome::partially_completed(
            vec![useful.clone()],
            namespace_failure.clone(),
        )
        .is_err()
    );
    assert!(CorrosionDeployOutcome::failed(vec![failed.clone()], namespace_failure).is_ok());
    assert!(
        CorrosionDeployOutcome::failed(vec![useful.clone()], CorrosionDeployFailure::Interrupted,)
            .is_err()
    );
    assert!(
        CorrosionDeployOutcome::cancelled(
            vec![useful],
            CorrosionDeployCancellationReason::Shutdown,
        )
        .is_ok()
    );
    assert!(
        CorrosionDeployOutcome::cancelled(
            vec![failed],
            CorrosionDeployCancellationReason::Shutdown,
        )
        .is_err()
    );
}

#[test]
fn deploy_recovery_failures_have_closed_durable_wire_shapes() {
    let evidence_lost = CorrosionDeployFailure::EvidenceLost {
        reason: CorrosionEvidenceLossReason::IdentityMismatch,
    };
    assert_eq!(
        serde_json::to_value(evidence_lost).expect("evidence-loss JSON"),
        json!({ "kind": "evidence_lost", "reason": "identity_mismatch" })
    );

    let promotion = CorrosionDeployFailure::Promotion {
        service_id: service_id(),
        failure: CorrosionPromotionFailure::InvariantViolation {
            service_row: CorrosionPromotionRowObservation::Exact,
            container_row: CorrosionPromotionRowObservation::Mismatch,
        },
    };
    assert_eq!(
        serde_json::to_value(promotion).expect("promotion JSON"),
        json!({
            "kind": "promotion",
            "service_id": SERVICE,
            "failure": {
                "kind": "invariant_violation",
                "service_row": "exact",
                "container_row": "mismatch"
            }
        })
    );
}

#[test]
fn deploy_transition_rejects_timestamp_regression_and_incomplete_results() {
    let timestamp_error = created_deploy()
        .transition_deploy(CorrosionDeployTransition::Running {
            started_at: timestamp("2026-08-05T09:59:59Z"),
        })
        .expect_err("start cannot precede creation");
    assert_eq!(
        timestamp_error
            .document()
            .deploy_targets()
            .expect("targets")
            .len(),
        1
    );

    let other_service = ServiceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FB0").expect("service id");
    let result_error = created_deploy()
        .transition_deploy(CorrosionDeployTransition::Terminal {
            completed_at: timestamp("2026-08-05T10:00:02Z"),
            outcome: CorrosionDeployOutcome::completed(vec![
                CorrosionDeployServiceResult::completed(other_service),
            ])
            .expect("completed outcome"),
        })
        .expect_err("results must exactly match targets");
    assert_eq!(
        result_error
            .document()
            .deploy_targets()
            .expect("targets")
            .len(),
        1
    );
}

#[test]
fn operation_document_deserialization_rejects_illegal_deploy_combinations() {
    let missing_result = json!({
        "v": 1,
        "cluster_id": CLUSTER,
        "machine_id": MACHINE,
        "kind": "deploy",
        "namespace_id": NAMESPACE,
        "service_ids": [SERVICE],
        "initiator": { "kind": "peer", "peer_id": PEER },
        "state": "terminal",
        "created_at": "2026-08-05T10:00:00Z",
        "started_at": null,
        "completed_at": "2026-08-05T10:00:02Z",
        "outcome": "completed",
        "results": []
    });
    assert!(serde_json::from_value::<OperationDocument>(missing_result).is_err());

    let deploy_state_on_machine_remove = json!({
        "v": 1,
        "cluster_id": CLUSTER,
        "machine_id": MACHINE,
        "kind": "machine_remove",
        "target_machine_id": MACHINE,
        "initiator": { "kind": "peer", "peer_id": PEER },
        "state": "terminal",
        "created_at": "2026-08-05T10:00:00Z",
        "started_at": null,
        "completed_at": "2026-08-05T10:00:02Z",
        "outcome": "completed",
        "results": [{ "service_id": SERVICE, "result": "completed" }]
    });
    assert!(serde_json::from_value::<OperationDocument>(deploy_state_on_machine_remove).is_err());
}

#[test]
fn operation_row_identity_remains_distinct_from_operation_subject_identity() {
    let row_id = OperationRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FB1").expect("operation row id");
    assert_eq!(row_id.as_str(), "01ARZ3NDEKTSV4RRFFQ69G5FB1");
}
