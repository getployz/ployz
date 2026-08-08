use ployz_core::RouteRemoveRequest;
use ployz_core::corrosion::{
    CorrosionAutomaticRouteFailure, CorrosionBasicOperation, CorrosionBasicTransition,
    CorrosionDeployCancellationReason, CorrosionDeployFailure, CorrosionDeployOutcome,
    CorrosionDeployOutcomeError, CorrosionDeployServiceFailure, CorrosionDeployServiceResult,
    CorrosionDeployState, CorrosionDeployTargets, CorrosionDeployTransition,
    CorrosionDocumentVersion, CorrosionEvidenceLossReason, CorrosionOperation,
    CorrosionOperationFailure, CorrosionOperationState, CorrosionPromotionFailure,
    CorrosionPromotionRowObservation, CorrosionTimestamp, DEPLOY_CLAIM_STALE_AFTER,
    DEPLOY_HEARTBEAT_INTERVAL, DeployClaim, DeployLiveness, DeployTakeover, OperationDocument,
    OperationInitiator, adjudicate_deploy_claim, check_deploy_takeover,
};
use ployz_core::ids::{
    ClusterId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, RouteBindingRowId,
    ServiceRowId,
};
use ployz_core::operation::RouteHostname;
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
            "results": [{ "service_id": SERVICE, "result": "completed" }],
            "heartbeat_at": "2026-08-05T10:00:00.000000000Z"
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
fn automatic_route_collision_preserves_the_exact_removal_handoff() {
    let hostname = RouteHostname::try_new("web.example.com").expect("hostname");
    let route_id = RouteBindingRowId::try_new(CLUSTER).expect("route id");
    let failure = CorrosionDeployFailure::AutomaticRoute {
        service_id: service_id(),
        failure: CorrosionAutomaticRouteFailure::HostnameCollision {
            hostname: hostname.clone(),
            route_id: route_id.clone(),
            remove: RouteRemoveRequest {
                hostname,
                route_id: Some(route_id),
            },
        },
    };

    assert_eq!(
        serde_json::to_value(failure).expect("automatic route failure JSON"),
        json!({
            "kind": "automatic_route",
            "service_id": SERVICE,
            "failure": {
                "kind": "hostname_collision",
                "hostname": "web.example.com",
                "route_id": CLUSTER,
                "remove": {"hostname": "web.example.com", "route_id": CLUSTER}
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

fn operation_row_id(value: &str) -> OperationRowId {
    OperationRowId::try_new(value).expect("operation row id")
}

fn deploy_created_at(created_at: &str) -> OperationDocument {
    OperationDocument::deploy_created(
        CorrosionDocumentVersion::V1,
        ClusterId::try_new(CLUSTER).expect("cluster id"),
        MachineRowId::try_new(MACHINE).expect("machine id"),
        OperationInitiator::Peer {
            peer_id: PeerId::try_new(PEER).expect("peer id"),
        },
        NamespaceRowId::try_new(NAMESPACE).expect("namespace id"),
        CorrosionDeployTargets::try_new(vec![service_id()]).expect("deploy targets"),
        timestamp(created_at),
    )
}

fn superseded_by(winner: &OperationRowId) -> OperationDocument {
    deploy_created_at("2026-08-05T10:00:00Z")
        .transition_deploy(CorrosionDeployTransition::Terminal {
            completed_at: timestamp("2026-08-05T10:00:01Z"),
            outcome: CorrosionDeployOutcome::failed(
                vec![CorrosionDeployServiceResult::skipped(service_id())],
                CorrosionDeployFailure::SupersededByOperation {
                    winner: winner.clone(),
                },
            )
            .expect("superseded outcome"),
        })
        .expect("terminal transition")
}

fn failed_interrupted() -> OperationDocument {
    deploy_created_at("2026-08-05T10:00:00Z")
        .transition_deploy(CorrosionDeployTransition::Terminal {
            completed_at: timestamp("2026-08-05T10:00:01Z"),
            outcome: CorrosionDeployOutcome::failed(
                vec![CorrosionDeployServiceResult::skipped(service_id())],
                CorrosionDeployFailure::Interrupted,
            )
            .expect("interrupted outcome"),
        })
        .expect("terminal transition")
}

#[test]
fn heartbeat_starts_at_creation_and_survives_transitions() {
    let created = deploy_created_at("2026-08-05T10:00:00Z");
    assert_eq!(
        created.heartbeat_at(),
        Some(timestamp("2026-08-05T10:00:00Z"))
    );

    let running = created
        .transition_deploy(CorrosionDeployTransition::Running {
            started_at: timestamp("2026-08-05T10:00:01Z"),
        })
        .expect("running transition");
    assert_eq!(
        running.heartbeat_at(),
        Some(timestamp("2026-08-05T10:00:00Z"))
    );
}

#[test]
fn rows_without_a_heartbeat_deserialize_to_none_and_fall_back_to_created_at() {
    let document: OperationDocument = serde_json::from_value(json!({
        "v": 1,
        "cluster_id": CLUSTER,
        "machine_id": MACHINE,
        "kind": "deploy",
        "namespace_id": NAMESPACE,
        "service_ids": [SERVICE],
        "initiator": { "kind": "peer", "peer_id": PEER },
        "state": "created",
        "created_at": "2026-08-05T10:00:00Z"
    }))
    .expect("pre-heartbeat row deserializes");
    assert_eq!(document.heartbeat_at(), None);

    assert_eq!(
        document.deploy_liveness(timestamp("2026-08-05T10:00:59Z")),
        DeployLiveness::Live
    );
    assert_eq!(
        document.deploy_liveness(timestamp("2026-08-05T10:01:00Z")),
        DeployLiveness::Stale
    );
}

#[test]
fn liveness_uses_the_heartbeat_and_the_exact_stale_bound() {
    assert_eq!(DEPLOY_HEARTBEAT_INTERVAL.as_secs(), 15);
    assert_eq!(DEPLOY_CLAIM_STALE_AFTER.as_secs(), 60);

    let refreshed = deploy_created_at("2026-08-05T10:00:00Z")
        .refresh_heartbeat(timestamp("2026-08-05T10:05:00Z"))
        .expect("nonterminal refresh");
    assert_eq!(
        refreshed.deploy_liveness(timestamp("2026-08-05T10:05:59.999999999Z")),
        DeployLiveness::Live
    );
    assert_eq!(
        refreshed.deploy_liveness(timestamp("2026-08-05T10:06:00Z")),
        DeployLiveness::Stale
    );
    assert!(refreshed.is_stalled(timestamp("2026-08-05T10:06:00Z")));
    assert!(!refreshed.is_stalled(timestamp("2026-08-05T10:05:30Z")));
}

#[test]
fn refresh_heartbeat_changes_only_the_heartbeat_and_refuses_terminal() {
    let created = deploy_created_at("2026-08-05T10:00:00Z");
    let expected_operation = created.operation().clone();
    let refreshed = created
        .refresh_heartbeat(timestamp("2026-08-05T10:00:30Z"))
        .expect("nonterminal refresh");
    assert_eq!(refreshed.operation(), &expected_operation);
    assert_eq!(
        refreshed.heartbeat_at(),
        Some(timestamp("2026-08-05T10:00:30Z"))
    );

    let terminal = failed_interrupted();
    let expected = terminal.clone();
    let error = terminal
        .refresh_heartbeat(timestamp("2026-08-05T10:00:30Z"))
        .expect_err("terminal is final");
    assert_eq!(error.document(), &expected);

    let succeeded_build = created_build()
        .transition_basic(CorrosionBasicTransition::Succeeded {
            completed_at: timestamp("2026-08-05T10:00:02Z"),
        })
        .expect("terminal transition");
    assert!(
        succeeded_build
            .refresh_heartbeat(timestamp("2026-08-05T10:00:30Z"))
            .is_err()
    );
}

#[test]
fn claim_is_lost_to_the_lowest_live_ulid_only() {
    let mine = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB5");
    let lower_live = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB2");
    let lowest_live = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB1");
    let higher_live = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB9");
    let now = timestamp("2026-08-05T10:00:10Z");
    let live = || deploy_created_at("2026-08-05T10:00:00Z");

    let candidates = vec![
        (lowest_live.clone(), live()),
        (lower_live.clone(), live()),
        (mine.clone(), live()),
        (higher_live.clone(), live()),
    ];
    assert_eq!(
        adjudicate_deploy_claim(&mine, &candidates, now),
        DeployClaim::Lost {
            winner: lowest_live,
        }
    );
    assert_eq!(
        adjudicate_deploy_claim(
            &operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB0"),
            &candidates,
            now
        ),
        DeployClaim::Won
    );
}

#[test]
fn stale_and_terminal_lower_ulids_never_block_a_claim() {
    let mine = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB5");
    let stale_lower = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB1");
    let terminal_lower = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB2");
    let now = timestamp("2026-08-05T10:02:00Z");

    let candidates = vec![
        (stale_lower, deploy_created_at("2026-08-05T10:00:00Z")),
        (terminal_lower, failed_interrupted()),
        (
            mine.clone(),
            deploy_created_at("2026-08-05T10:00:00Z")
                .refresh_heartbeat(now)
                .expect("nonterminal refresh"),
        ),
    ];
    assert_eq!(
        adjudicate_deploy_claim(&mine, &candidates, now),
        DeployClaim::Won
    );
}

#[test]
fn takeover_triggers_on_any_newer_op_not_terminally_superseded_to_me() {
    let mine = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB1");
    let newer_failed = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB2");
    let newer_running = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB3");

    assert_eq!(
        check_deploy_takeover(&mine, &[(newer_failed.clone(), failed_interrupted())]),
        DeployTakeover::TakenOver {
            winner: newer_failed,
        }
    );
    assert_eq!(
        check_deploy_takeover(
            &mine,
            &[(
                newer_running.clone(),
                deploy_created_at("2026-08-05T10:00:00Z")
            )]
        ),
        DeployTakeover::TakenOver {
            winner: newer_running,
        }
    );
}

#[test]
fn takeover_is_clear_when_every_newer_op_terminally_superseded_to_me() {
    let mine = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB1");
    let loser = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB2");
    let other = operation_row_id("01ARZ3NDEKTSV4RRFFQ69G5FB9");

    assert_eq!(
        check_deploy_takeover(&mine, &[(loser.clone(), superseded_by(&mine))]),
        DeployTakeover::Clear
    );
    assert_eq!(
        check_deploy_takeover(&mine, &[(loser.clone(), superseded_by(&other))]),
        DeployTakeover::TakenOver { winner: loser }
    );
}
