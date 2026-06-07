use ployz_core::dataplane::WireGuardEbpfComponent;
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{ContainerId, NodeId, OperationId, ServiceId};
use ployz_core::ops::{
    CancellationReason, DeployOperationFailure, DeployOperationState, DeployRunningStage,
    EventSequence, FailureMessage, HealthCheckFailure, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayPage, OperationEventReplayRequest, OperationStatus, OperatorHint,
    ReplayedOperationEvent, RetainedArtifact, RouteCutoverFailureReason, RouteHostname, RoutePort,
    RouteTarget,
};

#[test]
fn operation_state_serializes_with_stable_wire_names() {
    let state = DeployOperationState::Running {
        stage: active_service_running(),
    };

    assert_eq!(
        serde_json::to_string(&state).expect("state serializes"),
        r#"{"state":"running","stage":"active_service_commit"}"#
    );
}

#[test]
fn wireguard_ebpf_running_stage_has_stable_wire_name() {
    let state = DeployOperationState::Running {
        stage: DeployRunningStage::PreparingWireGuardEbpf,
    };

    assert_eq!(
        serde_json::to_string(&state).expect("state serializes"),
        r#"{"state":"running","stage":"preparing_wireguard_ebpf"}"#
    );
}

#[test]
fn running_operation_status_round_trips_through_json() {
    let status = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
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
        retained_artifacts: vec![RetainedArtifact::StartedContainer {
            node_id: node_id("node_7"),
            container_id: container_id("ctr_123"),
            log_hint: operator_hint("ployz logs ctr_123"),
        }],
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"health_check_failed","health_check":{"reason":"timed_out","timeout_seconds":30},"retained_artifacts":[{"type":"started_container","node_id":"node_7","container_id":"ctr_123","log_hint":"ployz logs ctr_123"}]}"#
    );
}

#[test]
fn wireguard_ebpf_failures_are_distinct_from_runtime_failures() {
    let failure = DeployOperationFailure::WireGuardEbpfUnavailable {
        node_id: node_id("node_7"),
        component: WireGuardEbpfComponent::EbpfForwarding,
        message: failure_message("bpf route install failed"),
        retained_artifacts: Vec::new(),
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"wireguard_ebpf_unavailable","node_id":"node_7","component":"ebpf_forwarding","message":"bpf route install failed","retained_artifacts":[]}"#
    );
}

#[test]
fn wireguard_ebpf_timeout_failures_keep_node_scope() {
    let failure = DeployOperationFailure::WireGuardEbpfPreparationTimedOut {
        nodes: vec![node_id("node_7"), node_id("node_8")],
        timeout_seconds: 30,
        retained_artifacts: Vec::new(),
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"wireguard_ebpf_preparation_timed_out","nodes":["node_7","node_8"],"timeout_seconds":30,"retained_artifacts":[]}"#
    );
}

#[test]
fn terminal_operation_state_is_explicit() {
    assert!(DeployOperationState::Completed.is_terminal());
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
        service_id("svc_api"),
        event_sequence(42),
    );

    assert_eq!(
        serde_json::to_string(&status).expect("status serializes"),
        r#"{"kind":"deploy","id":"op_123","service_id":"svc_api","state":{"state":"accepted"},"last_event_sequence":"42"}"#
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
        "service_id": "svc_api",
        "target_revision": "rev_1",
        "image": "",
        "replicas": 1
    }"#;

    assert!(serde_json::from_str::<DeployRequest>(empty_image).is_err());

    let zero_replicas = r#"{
        "service_id": "svc_api",
        "target_revision": "rev_1",
        "image": "ghcr.io/acme/api:rev-1",
        "replicas": 0
    }"#;

    assert!(serde_json::from_str::<DeployRequest>(zero_replicas).is_err());

    let whitespace_image = r#"{
        "service_id": "svc_api",
        "target_revision": "rev_1",
        "image": " ghcr.io/acme/api:rev-1",
        "replicas": 1
    }"#;

    assert!(serde_json::from_str::<DeployRequest>(whitespace_image).is_err());
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
            "node_id": "node_7",
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
        "service_id": "svc_api",
        "target_revision": "rev_1",
        "image": "ghcr.io/acme/api:rev-1",
        "replicas": 1,
        "unsupported": true
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
            "node_id": "node_7",
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
fn cancellation_reasons_are_non_empty() {
    let event = OperationEvent::Cancelled {
        operation_id: operation_id("op_123"),
        reason: cancellation_reason("operator cancelled"),
    };

    assert_eq!(
        serde_json::to_string(&event).expect("event serializes"),
        r#"{"event":"cancelled","operation_id":"op_123","reason":"operator cancelled"}"#
    );

    let empty_reason = r#"{
        "event": "cancelled",
        "operation_id": "op_123",
        "reason": ""
    }"#;

    assert!(serde_json::from_str::<OperationEvent>(empty_reason).is_err());
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ActiveServiceCommit
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

fn cancellation_reason(value: &str) -> CancellationReason {
    CancellationReason::try_new(value).expect("valid cancellation reason")
}

fn operator_hint(value: &str) -> OperatorHint {
    OperatorHint::try_new(value).expect("valid operator hint")
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::try_new(route_hostname(hostname), route_port(port))
}

fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}
