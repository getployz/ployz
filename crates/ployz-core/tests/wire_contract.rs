use ployz_core::cert::ManagedCertificateIssuanceFailureKind;
use ployz_core::dataplane::{
    DataplaneProviderFailure, EbpfForwardingReady, EbpfForwardingReadyEvidence,
    PloyzNativeMeshComponent, PloyzNativeMeshMachineReady, PloyzNativeMeshPrepareReport,
    PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::deploy::{DeployOrigin, DeployOriginError, DeployRequest};
use ployz_core::ops::{
    ArtifactUnavailableReason, ControlPlaneCommitScope, DeployFailureClass, DeployOperationFailure,
    DeployOperationState, DeployRunningStage, EventSequence, HealthCheckFailure,
    MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayLimit, OperationEventReplayPage, OperationEventReplayRequest,
    OperationKind, OperationStatus, OperatorHint, ReplayedOperationEvent, RetainedArtifact,
    RouteCutoverFailureReason, RouteTarget,
};
use ployz_core::state::MachineUsabilityReason;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    cancellation_reason, container_id, event_replay_limit, event_sequence, failure_message,
    machine_id, namespace_id, operation_id, route_hostname, route_port, service_id,
};

#[test]
fn operation_state_serializes_with_stable_wire_names() {
    let state = DeployOperationState::Running {
        stage: active_service_running(),
    };

    assert_eq!(
        serde_json::to_string(&state).expect("state serializes"),
        r#"{"state":"running","stage":"serving_target_commit"}"#
    );
}

#[test]
fn dataplane_running_stage_has_stable_wire_name() {
    let state = DeployOperationState::Running {
        stage: DeployRunningStage::PreparingDataplane,
    };

    assert_eq!(
        serde_json::to_string(&state).expect("state serializes"),
        r#"{"state":"running","stage":"preparing_dataplane"}"#
    );
}

#[test]
fn dataplane_prepared_event_has_stable_wire_shape() {
    let event = OperationEvent::DeployDataplanePrepared {
        operation_id: operation_id("op_123"),
        report: PloyzNativeMeshPrepareReport::for_targets(
            &[machine_id("machine_7")],
            [PloyzNativeMeshMachineReady {
                machine_id: machine_id("machine_7"),
                ready: PloyzNativeMeshReady {
                    wireguard: WireGuardReady {
                        public_key: wireguard_public_key("test-public-key"),
                        evidence: vec![WireGuardReadyEvidence::Command {
                            program: "wg".to_owned(),
                            args: vec!["--version".to_owned()],
                        }],
                    },
                    ebpf_forwarding: EbpfForwardingReady {
                        evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                            path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                            symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
                        }],
                    },
                },
            }],
        )
        .expect("valid report"),
    };

    assert_eq!(
        serde_json::to_string(&event).expect("event serializes"),
        concat!(
            r#"{"event":"deploy_dataplane_prepared","operation_id":"op_123","#,
            r#""report":{"machines":[{"machine_id":"machine_7","#,
            r#""wireguard":{"public_key":"test-public-key","evidence":[{"kind":"command","program":"wg","args":["--version"]}]},"#,
            r#""ebpf_forwarding":{"evidence":[{"kind":"ployz_tc_bytecode","#,
            r#""path":"/usr/local/lib/ployz/ebpf/ployz-ebpf-tc","#,
            r#""symbols":["ployz_egress","ployz_ingress"]}]}}]}}"#
        )
    );
}

#[test]
fn waiting_for_managed_certificate_event_has_stable_wire_shape() {
    let event = OperationEvent::DeployWaitingForManagedCertificate {
        operation_id: operation_id("op_123"),
    };

    assert_eq!(
        serde_json::to_string(&event).expect("event serializes"),
        r#"{"event":"deploy_waiting_for_managed_certificate","operation_id":"op_123"}"#
    );
}

#[test]
fn certificate_pending_failure_carries_the_latest_worker_error() {
    let failure = DeployOperationFailure::CertificatePending {
        last_error: Some(ManagedCertificateIssuanceFailureKind::ValidationTimeout),
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"certificate_pending","last_error":"validation_timeout"}"#
    );
    assert_eq!(failure.failure_class(), DeployFailureClass::Timeout);
}

#[test]
fn route_cutover_running_stage_has_stable_wire_name() {
    let state = DeployOperationState::Running {
        stage: DeployRunningStage::RouteCutover,
    };

    assert_eq!(
        serde_json::to_string(&state).expect("state serializes"),
        r#"{"state":"running","stage":"route_cutover"}"#
    );
}

#[test]
fn removing_superseded_containers_stage_has_stable_wire_name() {
    let state = DeployOperationState::Running {
        stage: DeployRunningStage::RemovingSupersededContainers,
    };

    assert_eq!(
        serde_json::to_string(&state).expect("state serializes"),
        r#"{"state":"running","stage":"removing_superseded_containers"}"#
    );
}

#[test]
fn completed_operation_state_carries_stable_outcome_name() {
    assert_eq!(
        serde_json::to_string(&DeployOperationState::completed()).expect("state serializes"),
        r#"{"state":"completed","outcome":"completed"}"#
    );
}

#[test]
fn running_operation_status_round_trips_through_json() {
    let status = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: event_sequence(2),
    };

    let json = serde_json::to_string(&status).expect("status serializes");
    assert_eq!(
        serde_json::from_str::<OperationStatus>(&json).expect("status deserializes"),
        status,
    );
}

#[test]
fn retained_artifact_carries_variant_specific_failure_data() {
    let failure = DeployOperationFailure::HealthCheckFailed {
        health_check: HealthCheckFailure::TimedOut {
            timeout_seconds: 30,
        },
        retained_artifacts: vec![
            RetainedArtifact::StartedContainer {
                machine_id: machine_id("machine_7"),
                container_id: container_id("ctr_123"),
                log_hint: operator_hint("ployz logs ctr_123"),
            },
            RetainedArtifact::ContainerStopFailed {
                machine_id: machine_id("machine_7"),
                container_id: container_id("ctr_123"),
                message: failure_message("container stop failed"),
                inspect_hint: operator_hint("ployz container inspect ctr_123"),
            },
        ],
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"health_check_failed","health_check":{"reason":"timed_out","timeout_seconds":30},"retained_artifacts":[{"type":"started_container","machine_id":"machine_7","container_id":"ctr_123","log_hint":"ployz logs ctr_123"},{"type":"container_stop_failed","machine_id":"machine_7","container_id":"ctr_123","message":"container stop failed","inspect_hint":"ployz container inspect ctr_123"}]}"#
    );
}

#[test]
fn dataplane_failures_are_distinct_from_runtime_failures() {
    let failure = DeployOperationFailure::DataplaneUnavailable {
        machine_id: machine_id("machine_7"),
        provider_failure: DataplaneProviderFailure::PloyzNativeMesh {
            component: PloyzNativeMeshComponent::EbpfForwarding,
        },
        message: failure_message("bpf route install failed"),
        retained_artifacts: Vec::new(),
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"dataplane_unavailable","machine_id":"machine_7","provider_failure":{"provider":"ployz_native_mesh","component":"ebpf_forwarding"},"message":"bpf route install failed","retained_artifacts":[]}"#
    );
}

#[test]
fn dataplane_timeout_failures_keep_machine_scope() {
    let failure = DeployOperationFailure::DataplanePrepareTimedOut {
        machines: vec![machine_id("machine_7"), machine_id("machine_8")],
        timeout_seconds: 30,
        retained_artifacts: Vec::new(),
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"dataplane_prepare_timed_out","machines":["machine_7","machine_8"],"timeout_seconds":30,"retained_artifacts":[]}"#
    );
}

#[test]
fn container_start_failures_keep_container_scope() {
    let failure = DeployOperationFailure::ContainerStartFailed {
        machine_id: machine_id("machine_7"),
        container_id: container_id("ctr_123"),
        message: failure_message("container start failed"),
        retained_artifacts: vec![RetainedArtifact::CreatedContainer {
            machine_id: machine_id("machine_7"),
            container_id: container_id("ctr_123"),
            inspect_hint: operator_hint("ployzctl container inspect ctr_123"),
        }],
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"container_start_failed","machine_id":"machine_7","container_id":"ctr_123","message":"container start failed","retained_artifacts":[{"type":"created_container","machine_id":"machine_7","container_id":"ctr_123","inspect_hint":"ployzctl container inspect ctr_123"}]}"#
    );
}

#[test]
fn deploy_failures_map_to_closed_failure_classes() {
    let cases = [
        (
            DeployOperationFailure::NoUsableMachines {
                reasons: vec![ployz_core::ops::UnusableMachine {
                    machine_id: machine_id("machine_7"),
                    reason: MachineUsabilityReason::Draining,
                }],
            },
            DeployFailureClass::PreconditionRejected,
        ),
        (
            DeployOperationFailure::NoUsableMachines {
                reasons: vec![ployz_core::ops::UnusableMachine {
                    machine_id: machine_id("machine_7"),
                    reason: MachineUsabilityReason::FactsUnavailable,
                }],
            },
            DeployFailureClass::MachineNoAnswer,
        ),
        (
            DeployOperationFailure::PlanningFailed {
                service_id: service_id("svc_api"),
                namespace_revision_id: ployz_test_support::ids::namespace_revision_id("rev_1"),
                message: failure_message("invalid deploy input"),
            },
            DeployFailureClass::PreconditionRejected,
        ),
        (
            DeployOperationFailure::ArtifactUnavailable {
                service_id: service_id("svc_api"),
                namespace_revision_entry_id: ployz_test_support::ids::namespace_revision_entry_id(
                    "entry_1",
                ),
                reason: ArtifactUnavailableReason::BundleMissing,
            },
            DeployFailureClass::ImageResolvePullFailed,
        ),
        (
            DeployOperationFailure::DataplaneUnavailable {
                machine_id: machine_id("machine_7"),
                provider_failure: DataplaneProviderFailure::PloyzNativeMesh {
                    component: PloyzNativeMeshComponent::EbpfForwarding,
                },
                message: failure_message("dataplane unavailable"),
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::DataplanePrepareFailed,
        ),
        (
            DeployOperationFailure::DataplanePrepareTimedOut {
                machines: vec![machine_id("machine_7")],
                timeout_seconds: 30,
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::Timeout,
        ),
        (
            DeployOperationFailure::DataplanePrepareInvalidReport {
                message: failure_message("invalid dataplane report"),
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::DataplanePrepareFailed,
        ),
        (
            DeployOperationFailure::RuntimeUnavailable {
                machine_id: machine_id("machine_7"),
                message: failure_message("runtime unavailable"),
                retained_artifacts: vec![RetainedArtifact::CreatedContainer {
                    machine_id: machine_id("machine_8"),
                    container_id: container_id("ctr_unrelated"),
                    inspect_hint: operator_hint("ployzctl container inspect ctr_unrelated"),
                }],
            },
            DeployFailureClass::RuntimeUnavailable,
        ),
        (
            DeployOperationFailure::ContainerStartFailed {
                machine_id: machine_id("machine_7"),
                container_id: container_id("ctr_123"),
                message: failure_message("container start failed"),
                retained_artifacts: vec![RetainedArtifact::CreatedContainer {
                    machine_id: machine_id("machine_7"),
                    container_id: container_id("ctr_123"),
                    inspect_hint: operator_hint("ployzctl container inspect ctr_123"),
                }],
            },
            DeployFailureClass::ContainerStartFailed,
        ),
        (
            DeployOperationFailure::HealthCheckFailed {
                health_check: HealthCheckFailure::ProbeFailed {
                    machine_id: machine_id("machine_7"),
                    container_id: container_id("ctr_123"),
                    message: failure_message("probe failed"),
                    log_hint: operator_hint("ployzctl logs ctr_123"),
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::HealthGateFailed,
        ),
        (
            DeployOperationFailure::HealthCheckFailed {
                health_check: HealthCheckFailure::TimedOut {
                    timeout_seconds: 30,
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::Timeout,
        ),
        (
            DeployOperationFailure::ControlPlaneCommitFailed {
                scope: ControlPlaneCommitScope::ServiceEntry {
                    service_id: service_id("svc_api"),
                    namespace_revision_entry_id:
                        ployz_test_support::ids::namespace_revision_entry_id("entry_1"),
                },
                message: failure_message("commit failed"),
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::ControlPlaneCommitFailed,
        ),
        (
            DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com", 443),
                reason: RouteCutoverFailureReason::GatewayUnavailable {
                    machine_id: machine_id("machine_7"),
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::MachineNoAnswer,
        ),
        (
            DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com", 443),
                reason: RouteCutoverFailureReason::RouteRejected {
                    message: failure_message("route rejected"),
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::RouteCutoverFailed,
        ),
        (
            DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com", 443),
                reason: RouteCutoverFailureReason::StateStoreFailed {
                    message: failure_message("state store failed"),
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::RouteCutoverFailed,
        ),
        (
            DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com", 443),
                reason: RouteCutoverFailureReason::TimedOut {
                    timeout_seconds: 30,
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::Timeout,
        ),
    ];

    for (failure, expected) in cases {
        assert_eq!(failure.failure_class(), expected);
    }
}

#[test]
fn terminal_operation_state_is_explicit() {
    assert!(DeployOperationState::completed().is_terminal());
    assert!(
        DeployOperationState::Failed {
            failure: DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com", 443),
                reason: RouteCutoverFailureReason::RouteRejected {
                    message: failure_message("route rejected"),
                },
                retained_artifacts: Vec::new(),
            },
        }
        .is_terminal()
    );
    assert!(
        !DeployOperationState::Running {
            stage: active_service_running(),
        }
        .is_terminal()
    );
}

#[test]
fn operation_status_subject_is_variant_specific_data() {
    let status = OperationStatus::deploy_accepted(
        operation_id("op_123"),
        namespace_id("default"),
        service_id("svc_api"),
        None,
        event_sequence(42),
    );

    assert_eq!(
        serde_json::to_string(&status).expect("status serializes"),
        r#"{"kind":"deploy","id":"op_123","namespace_id":"default","service_id":"svc_api","state":{"state":"accepted"},"last_event_sequence":"42"}"#
    );
}

#[test]
fn operation_event_replay_request_round_trips_through_json() {
    let request = OperationEventReplayRequest {
        operation_id: operation_id("op_123"),
        start_sequence: event_sequence(3),
        limit: event_replay_limit(50),
    };

    let json = serde_json::to_string(&request).expect("request serializes");

    assert_eq!(
        serde_json::from_str::<OperationEventReplayRequest>(&json).expect("request deserializes"),
        request,
    );
    assert_eq!(
        json,
        r#"{"operation_id":"op_123","start_sequence":"3","limit":50}"#
    );
}

#[test]
fn operation_event_replay_page_carries_explicit_cursor() {
    let page = OperationEventReplayPage::more(
        vec![ReplayedOperationEvent {
            sequence: event_sequence(4),
            event: OperationEvent::DeployPlanningStarted {
                operation_id: operation_id("op_123"),
            },
        }],
        event_sequence(5),
    );

    assert_eq!(
        serde_json::to_string(&page).expect("page serializes"),
        r#"{"events":[{"sequence":"4","event":{"event":"deploy_planning_started","operation_id":"op_123"}}],"cursor":{"state":"more","next_start_sequence":"5"}}"#
    );
    assert_eq!(
        OperationEventReplayPage::caught_up(Vec::new()).cursor,
        OperationEventReplayCursor::CaughtUp
    );
}

#[test]
fn operation_event_replay_limit_rejects_zero_and_oversized_wire_values() {
    assert!(serde_json::from_str::<OperationEventReplayLimit>("0").is_err());
    assert!(
        serde_json::from_str::<OperationEventReplayLimit>(
            &(MAX_OPERATION_EVENT_REPLAY_LIMIT + 1).to_string(),
        )
        .is_err()
    );
}

#[test]
fn public_u64_wire_values_are_string_encoded_without_narrowing_core_values() {
    let max_sequence = EventSequence::try_new(u64::MAX).expect("max u64 is valid internally");

    assert_eq!(
        serde_json::to_string(&max_sequence).expect("sequence serializes"),
        r#""18446744073709551615""#
    );
    assert_eq!(
        serde_json::from_str::<EventSequence>(r#""18446744073709551615""#)
            .expect("sequence deserializes")
            .get(),
        u64::MAX
    );
    assert!(serde_json::from_str::<EventSequence>("1").is_err());
}

#[test]
fn deploy_request_rejects_empty_image_and_zero_replicas() {
    let empty_image = r#"{
        "namespace_id": "default",
        "namespace_revision_id": "rev_1",
        "services": [{
            "service_id": "svc_api",
            "image": "",
            "replicas": 1
        }]
    }"#;

    assert!(serde_json::from_str::<DeployRequest>(empty_image).is_err());

    let zero_replicas = r#"{
        "namespace_id": "default",
        "namespace_revision_id": "rev_1",
        "services": [{
            "service_id": "svc_api",
            "image": "ghcr.io/acme/api:rev-1",
            "replicas": 0
        }]
    }"#;

    assert!(serde_json::from_str::<DeployRequest>(zero_replicas).is_err());

    let whitespace_image = r#"{
        "namespace_id": "default",
        "namespace_revision_id": "rev_1",
        "services": [{
            "service_id": "svc_api",
            "image": " ghcr.io/acme/api:rev-1",
            "replicas": 1
        }]
    }"#;

    assert!(serde_json::from_str::<DeployRequest>(whitespace_image).is_err());
}

#[test]
fn deploy_origin_is_a_single_bounded_caption() {
    assert_eq!(
        DeployOrigin::try_new("rollback to last good")
            .expect("ordinary caption is accepted")
            .as_str(),
        "rollback to last good"
    );
    assert_eq!(DeployOrigin::try_new("\n"), Err(DeployOriginError::Empty));
    assert!(DeployOrigin::try_new("é".repeat(64)).is_ok());
    assert_eq!(
        DeployOrigin::try_new("é".repeat(65)),
        Err(DeployOriginError::TooLong { bytes: 130 })
    );
    assert_eq!(
        DeployOrigin::try_new("rollback\nto last good"),
        Err(DeployOriginError::ControlCharacter)
    );
    assert!(serde_json::from_str::<DeployOrigin>(r#""rollback\nto last good""#).is_err());
}

#[test]
fn deploy_origin_round_trips_on_request_and_status() {
    let origin = DeployOrigin::try_new("manual release").expect("valid deploy origin");
    let request = DeployRequest {
        namespace_id: namespace_id("default"),
        origin: Some(origin.clone()),
        services: Vec::new(),
    };
    let status = OperationStatus::deploy_accepted(
        operation_id("op_origin"),
        namespace_id("default"),
        service_id("default"),
        Some(origin),
        event_sequence(1),
    );

    let request_json = serde_json::to_string(&request).expect("request serializes");
    let status_json = serde_json::to_string(&status).expect("status serializes");

    assert_eq!(
        serde_json::from_str::<DeployRequest>(&request_json).expect("request deserializes"),
        request
    );
    assert_eq!(
        serde_json::from_str::<OperationStatus>(&status_json).expect("status deserializes"),
        status
    );
}

#[test]
fn operation_status_rejects_missing_or_zero_event_sequence() {
    let missing_sequence = r#"{
        "kind": "deploy",
        "id": "op_123",
        "service_id": "svc_api",
        "state": { "state": "accepted" }
    }"#;

    assert!(serde_json::from_str::<OperationStatus>(missing_sequence).is_err());

    let zero_sequence = r#"{
        "kind": "deploy",
        "id": "op_123",
        "service_id": "svc_api",
        "state": { "state": "accepted" },
        "last_event_sequence": "0"
    }"#;

    assert!(serde_json::from_str::<OperationStatus>(zero_sequence).is_err());
}

#[test]
fn failure_payloads_reject_empty_operator_text() {
    let empty_log_hint = r#"{
        "kind": "health_check_failed",
        "health_check": {
            "reason": "probe_failed",
            "machine_id": "machine_7",
            "container_id": "ctr_123",
            "message": "health check failed",
            "log_hint": ""
        },
        "retained_artifacts": []
    }"#;

    assert!(serde_json::from_str::<DeployOperationFailure>(empty_log_hint).is_err());
}

#[test]
fn route_cutover_failures_use_structured_route_targets() {
    let failure = DeployOperationFailure::RouteCutoverFailed {
        route: route_target("api.example.com", 443),
        reason: RouteCutoverFailureReason::RouteRejected {
            message: failure_message("route rejected"),
        },
        retained_artifacts: Vec::new(),
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"route_cutover_failed","route":{"hostname":"api.example.com","port":443},"reason":{"reason":"route_rejected","message":"route rejected"},"retained_artifacts":[]}"#
    );

    let invalid_hostname = r#"{
        "kind": "route_cutover_failed",
        "route": { "hostname": "-api.example.com", "port": 443 },
        "reason": { "reason": "route_rejected", "message": "route rejected" },
        "retained_artifacts": []
    }"#;

    assert!(serde_json::from_str::<DeployOperationFailure>(invalid_hostname).is_err());

    let invalid_port = r#"{
        "kind": "route_cutover_failed",
        "route": { "hostname": "api.example.com", "port": 0 },
        "reason": { "reason": "route_rejected", "message": "route rejected" },
        "retained_artifacts": []
    }"#;

    assert!(serde_json::from_str::<DeployOperationFailure>(invalid_port).is_err());
}

#[test]
fn wire_models_reject_unknown_fields() {
    let deploy_with_extra = r#"{
        "namespace_id": "default",
        "namespace_revision_id": "rev_1",
        "services": [{
            "service_id": "svc_api",
            "image": "ghcr.io/acme/api:rev-1",
            "replicas": 1,
            "unsupported": true
        }]
    }"#;

    assert!(serde_json::from_str::<DeployRequest>(deploy_with_extra).is_err());

    let status_with_extra = r#"{
        "kind": "deploy",
        "id": "op_123",
        "service_id": "svc_api",
        "state": { "state": "accepted" },
        "last_event_sequence": "1",
        "unsupported": true
    }"#;

    assert!(serde_json::from_str::<OperationStatus>(status_with_extra).is_err());

    let failure_with_extra = r#"{
        "kind": "health_check_failed",
        "health_check": { "reason": "timed_out", "timeout_seconds": 30, "message": "stale" },
        "retained_artifacts": [{
            "type": "started_container",
            "machine_id": "machine_7",
            "container_id": "ctr_123",
            "log_hint": "ployz logs ctr_123"
        }]
    }"#;

    assert!(serde_json::from_str::<DeployOperationFailure>(failure_with_extra).is_err());
}

#[test]
fn root_operation_state_rejects_unknown_fields() {
    let state_with_extra = r#"{
        "state": "running",
        "stage": { "stage": "starting_containers" },
        "unsupported": true
    }"#;

    assert!(serde_json::from_str::<DeployOperationState>(state_with_extra).is_err());
}

#[test]
fn root_operation_event_rejects_unknown_fields() {
    let event_with_extra = r#"{
        "event": "deploy_planning_started",
        "operation_id": "op_123",
        "unsupported": true
    }"#;

    assert!(serde_json::from_str::<OperationEvent>(event_with_extra).is_err());
}

#[test]
fn machine_lifecycle_submitted_wire_shape_is_pinned() {
    let event = OperationEvent::MachineLifecycleSubmitted {
        operation_id: operation_id("op_123"),
        machine_id: machine_id("machine_7"),
        target: ployz_core::state::MachineLifecycle::Draining,
    };

    assert_eq!(
        serde_json::to_string(&event).expect("event serializes"),
        r#"{"event":"machine_lifecycle_submitted","operation_id":"op_123","machine_id":"machine_7","target":"draining"}"#
    );
}

#[test]
fn cancellation_reasons_are_non_empty() {
    let event = OperationEvent::Cancelled {
        operation_id: operation_id("op_123"),
        kind: OperationKind::Deploy,
        reason: cancellation_reason("operator cancelled"),
    };

    assert_eq!(
        serde_json::to_string(&event).expect("event serializes"),
        r#"{"event":"cancelled","operation_id":"op_123","kind":"deploy","reason":"operator cancelled"}"#
    );

    let empty_reason = r#"{
        "event": "cancelled",
        "operation_id": "op_123",
        "kind": "deploy",
        "reason": ""
    }"#;

    assert!(serde_json::from_str::<OperationEvent>(empty_reason).is_err());
}

fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ServingTargetCommit
}

fn wireguard_public_key(value: &str) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
}

fn operator_hint(value: &str) -> OperatorHint {
    OperatorHint::try_new(value).expect("valid operator hint")
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

#[test]
fn managed_container_observation_wire_shape_nests_identity() {
    // Observations persist in KV with deny_unknown_fields: this pin is the
    // wire contract for the nested identity shape.
    let observation = ployz_core::machine_runtime::ManagedContainerObservation {
        machine_id: machine_id("machine_a"),
        container_id: container_id("ctr_1"),
        identity: containers::identity("svc_api")
            .entry("entry_1")
            .operation("op_1")
            .step("step_1")
            .build(),
        state: ployz_core::machine_runtime::ContainerRuntimeState::Running {
            ip: None,
            health: ployz_core::machine_runtime::ContainerHealth::None,
            started_at_unix_ms: Some(1_783_670_950_123),
        },
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
    };

    assert_eq!(
        serde_json::to_value(&observation).expect("observation serializes"),
        serde_json::json!({
            "machine_id": "machine_a",
            "container_id": "ctr_1",
            "identity": {
                "namespace_id": "default",
                "service_id": "svc_api",
                "namespace_revision_entry_id": "entry_1",
                "operation_id": "op_1",
                "step_id": "step_1",
                "kind": "service",
            },
            "state": {
                "state": "running",
                "health": "none",
                "started_at_unix_ms": 1_783_670_950_123_u64,
            },
        })
    );
}

#[test]
fn deploy_cleanup_container_wire_shape_nests_identity() {
    // Cleanup containers ride operation events; same nested-identity
    // wire contract.
    let cleanup = ployz_core::deploy::DeployCleanupContainer {
        machine_id: machine_id("machine_a"),
        container_id: container_id("ctr_old"),
        identity: containers::identity("svc_api")
            .entry("entry_old")
            .operation("op_old")
            .step("step_old")
            .build(),
    };

    assert_eq!(
        serde_json::to_value(&cleanup).expect("cleanup container serializes"),
        serde_json::json!({
            "machine_id": "machine_a",
            "container_id": "ctr_old",
            "identity": {
                "namespace_id": "default",
                "service_id": "svc_api",
                "namespace_revision_entry_id": "entry_old",
                "operation_id": "op_old",
                "step_id": "step_old",
                "kind": "service",
            },
        })
    );
}
