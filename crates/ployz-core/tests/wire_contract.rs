use std::collections::BTreeMap;

use ployz_core::deploy::{
    ContainerMountPath, ContainerRuntimeSpec, DatasetName, DatasetNameError, DependencyCondition,
    DeployOrigin, DeployOriginError, DeployRequest, DeployServiceSpec, ImageReference, ImageSource,
    NormalizedDeployRequest, ReplicaCount, ServiceDependency, ServiceVolumeMount,
    VolumeMaxSizeBytes, VolumeName, VolumeSpec, ZfsPoolName, ZfsPoolNameError,
};
use ployz_core::intent::{VolumeKind, VolumePinState};
use ployz_core::machine::MachineUsabilityReason;
use ployz_core::operation::{
    ArtifactUnavailableReason, ControlPlaneCommitScope, DeployFailureClass, DeployOperationFailure,
    DeployOperationState, DeployRunningStage, EventSequence, HealthCheckFailure,
    MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayLimit, OperationEventReplayPage, OperationEventReplayRequest,
    OperationKind, OperationStatus, OperatorHint, ReplayedOperationEvent, RetainedArtifact,
    RouteCutoverFailureReason, RouteTarget,
};
use ployz_test_support::containers;
use ployz_test_support::ids::{
    cancellation_reason, container_id, event_replay_limit, event_sequence, failure_message,
    machine_id, namespace_id, operation_event_recorded_at, operation_id, route_hostname,
    service_id,
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
                reasons: vec![ployz_core::operation::UnusableMachine {
                    machine_id: machine_id("machine_7"),
                    reason: MachineUsabilityReason::Draining,
                }],
            },
            DeployFailureClass::PreconditionRejected,
        ),
        (
            DeployOperationFailure::NoUsableMachines {
                reasons: vec![ployz_core::operation::UnusableMachine {
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
                route: route_target("api.example.com"),
                reason: RouteCutoverFailureReason::GatewayUnavailable {
                    machine_id: machine_id("machine_7"),
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::MachineNoAnswer,
        ),
        (
            DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com"),
                reason: RouteCutoverFailureReason::RouteRejected {
                    message: failure_message("route rejected"),
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::RouteCutoverFailed,
        ),
        (
            DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com"),
                reason: RouteCutoverFailureReason::StateStoreFailed {
                    message: failure_message("state store failed"),
                },
                retained_artifacts: Vec::new(),
            },
            DeployFailureClass::RouteCutoverFailed,
        ),
        (
            DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com"),
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
                route: route_target("api.example.com"),
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
            recorded_at_unix_ms: operation_event_recorded_at(1_784_116_800_123),
            event: OperationEvent::DeployPlanningStarted {
                operation_id: operation_id("op_123"),
            },
        }],
        event_sequence(5),
    );

    assert_eq!(
        serde_json::to_string(&page).expect("page serializes"),
        r#"{"events":[{"sequence":"4","recorded_at_unix_ms":"1784116800123","event":{"event":"deploy_planning_started","operation_id":"op_123"}}],"cursor":{"state":"more","next_start_sequence":"5"}}"#
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
        volumes: std::collections::BTreeMap::new(),
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
fn deploy_request_requires_a_declaration_for_every_mounted_volume() {
    let request = request_with_volume_mount(std::collections::BTreeMap::new());

    let error = NormalizedDeployRequest::try_new(request).expect_err("undeclared mount is invalid");

    assert_eq!(error.service_id, service_id("svc_api"));
    assert_eq!(error.volume_name, volume_name("data"));
}

#[test]
fn deploy_request_synthesizes_plain_declarations_for_legacy_inputs() {
    let mut request = request_with_volume_mount(std::collections::BTreeMap::new());

    request.synthesize_plain_volume_declarations();

    assert_eq!(
        request.volumes.get(&volume_name("data")),
        Some(&VolumeSpec::Plain)
    );
}

#[test]
fn normalized_service_requests_retain_mounted_volume_declarations() {
    let provisioned = VolumeSpec::Provisioned {
        max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
    };
    let request = request_with_volume_mount(BTreeMap::from([
        (volume_name("data"), provisioned.clone()),
        (volume_name("unused"), VolumeSpec::Plain),
    ]));

    let request = NormalizedDeployRequest::try_new(request).expect("request normalizes");
    let services = request.services();
    let [service] = services else {
        panic!("request has one service");
    };
    assert_eq!(
        service.volumes,
        BTreeMap::from([(volume_name("data"), provisioned)])
    );
}

#[test]
fn provisioned_volume_contract_round_trips_without_nullable_state() {
    let spec = VolumeSpec::Provisioned {
        max_size_bytes: VolumeMaxSizeBytes::try_new(10 * 1024 * 1024).expect("nonzero volume size"),
    };

    let json = serde_json::to_string(&spec).expect("volume spec serializes");

    assert_eq!(json, r#"{"kind":"provisioned","max_size_bytes":10485760}"#);
    assert_eq!(
        serde_json::from_str::<VolumeSpec>(&json).expect("volume spec decodes"),
        spec
    );
}

#[test]
fn volume_pin_legacy_json_defaults_to_plain_kind() {
    let pin = serde_json::from_str::<VolumePinState>(
        r#"{"namespace_id":"default","volume_name":"data","machine_id":"machine_a"}"#,
    )
    .expect("legacy pin decodes");

    assert_eq!(pin.kind, VolumeKind::Plain);
}

#[test]
fn provisioned_volume_pin_round_trips_with_dataset_and_size() {
    let pin = VolumePinState {
        namespace_id: namespace_id("default"),
        volume_name: volume_name("data"),
        machine_id: machine_id("machine_a"),
        kind: VolumeKind::Provisioned {
            dataset: DatasetName::for_volume(
                &ZfsPoolName::try_new("tank").expect("pool name"),
                &namespace_id("default"),
                &volume_name("data"),
            )
            .expect("dataset name fits"),
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("nonzero volume size"),
        },
    };

    let json = serde_json::to_string(&pin).expect("pin serializes");

    assert_eq!(
        serde_json::from_str::<VolumePinState>(&json).expect("pin decodes"),
        pin
    );
}

#[test]
fn dataset_name_rejects_a_full_name_over_the_zfs_budget() {
    let error = DatasetName::for_volume(
        &ZfsPoolName::try_new("p".repeat(220)).expect("pool name"),
        &namespace_id("default"),
        &volume_name("postgres_data"),
    )
    .expect_err("full dataset name exceeds the budget");

    assert!(matches!(
        error,
        DatasetNameError::NameBudgetExceeded { maximum: 255, .. }
    ));
}

#[test]
fn dataset_name_rejects_foreign_and_malformed_names() {
    assert!(matches!(
        DatasetName::try_new("foreign/arbitrary"),
        Err(DatasetNameError::NonCanonical { .. })
    ));
    assert!(matches!(
        DatasetName::try_new("tank/ployz/volumes/ployz-n07-default-v4-data"),
        Err(DatasetNameError::NonCanonical { .. })
    ));
    assert!(matches!(
        DatasetName::try_new("tank/ployz/volumes/ployz-n8-default-v4-data"),
        Err(DatasetNameError::NonCanonical { .. })
    ));
    assert!(serde_json::from_str::<DatasetName>(r#""foreign/arbitrary""#).is_err());
}

#[test]
fn zfs_pool_name_rejects_hierarchical_physical_roots() {
    assert!(matches!(
        ZfsPoolName::try_new("tank/foreign"),
        Err(ZfsPoolNameError::Hierarchical { .. })
    ));
}

#[test]
fn volume_declaration_changes_do_not_change_the_namespace_revision() {
    let plain =
        request_with_volume_mount(BTreeMap::from([(volume_name("data"), VolumeSpec::Plain)]));
    let provisioned = request_with_volume_mount(BTreeMap::from([(
        volume_name("data"),
        VolumeSpec::Provisioned {
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
        },
    )]));

    assert_eq!(
        plain.namespace_revision_id(),
        provisioned.namespace_revision_id()
    );
}

fn request_with_volume_mount(
    volumes: std::collections::BTreeMap<VolumeName, VolumeSpec>,
) -> DeployRequest {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name("data"),
        target: ContainerMountPath::try_new("/data").expect("mount path"),
    }];
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes,
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("nginx:latest").expect("image"),
            image_source: ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("replicas"),
            runtime,
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn volume_name(value: &str) -> VolumeName {
    VolumeName::try_new(value).expect("volume name")
}

#[test]
fn service_dependency_conditions_have_explicit_wire_values() {
    let started = ServiceDependency {
        service_id: service_id("database"),
        condition: DependencyCondition::Started,
    };
    let healthy = ServiceDependency {
        service_id: service_id("cache"),
        condition: DependencyCondition::Healthy,
    };

    assert_eq!(
        serde_json::to_string(&[started, healthy]).expect("dependencies serialize"),
        r#"[{"service_id":"database","condition":"started"},{"service_id":"cache","condition":"healthy"}]"#
    );
    assert!(
        serde_json::from_str::<ServiceDependency>(r#"{"service_id":"database"}"#).is_err(),
        "the core wire contract must not imply a dependency condition"
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
        route: route_target("api.example.com"),
        reason: RouteCutoverFailureReason::RouteRejected {
            message: failure_message("route rejected"),
        },
        retained_artifacts: Vec::new(),
    };

    assert_eq!(
        serde_json::to_string(&failure).expect("failure serializes"),
        r#"{"kind":"route_cutover_failed","route":{"hostname":"api.example.com"},"reason":{"reason":"route_rejected","message":"route rejected"},"retained_artifacts":[]}"#
    );

    let invalid_hostname = r#"{
        "kind": "route_cutover_failed",
        "route": { "hostname": "-api.example.com" },
        "reason": { "reason": "route_rejected", "message": "route rejected" },
        "retained_artifacts": []
    }"#;

    assert!(serde_json::from_str::<DeployOperationFailure>(invalid_hostname).is_err());

    let legacy_public_port = r#"{
        "kind": "route_cutover_failed",
        "route": { "hostname": "api.example.com", "port": 443 },
        "reason": { "reason": "route_rejected", "message": "route rejected" },
        "retained_artifacts": []
    }"#;

    assert!(serde_json::from_str::<DeployOperationFailure>(legacy_public_port).is_err());
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
        target: ployz_core::machine::MachineLifecycle::Draining,
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

fn operator_hint(value: &str) -> OperatorHint {
    OperatorHint::try_new(value).expect("valid operator hint")
}

fn route_target(hostname: &str) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname))
}

#[test]
fn managed_container_observation_wire_shape_nests_identity() {
    // Observations persist in KV with deny_unknown_fields: this pin is the
    // wire contract for the nested identity shape.
    let observation = ployz_core::machine::ManagedContainerObservation {
        machine_id: machine_id("machine_a"),
        container_id: container_id("ctr_1"),
        identity: containers::identity("svc_api")
            .entry("entry_1")
            .operation("op_1")
            .step("step_1")
            .build(),
        state: ployz_core::machine::ContainerRuntimeState::Running {
            ip: None,
            health: ployz_core::machine::ContainerHealth::None,
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
