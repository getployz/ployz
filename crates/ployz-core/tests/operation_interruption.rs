use ployz_core::ids::{NamespaceId, OperationId, ServiceId};
use ployz_core::operation::{
    DeployInterruptionStage, DeployOperationState, DeployRunningStage, EventSequence,
    OperationEvent, OperationInterruptionCause, OperationInterruptionNextAction,
    OperationInterruptionStage, OperationInterruptionUncertainWork, OperationProjection,
    OperationStatus, project_operation_event,
};

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("operation id")
}

fn namespace_id() -> NamespaceId {
    NamespaceId::try_new("production").expect("namespace id")
}

fn service_id() -> ServiceId {
    ServiceId::try_new("api").expect("service id")
}

#[test]
fn running_deploy_interruption_preserves_typed_stage_and_retry_contract() {
    let status = OperationStatus::Deploy {
        id: operation_id("op-deploy"),
        namespace_id: namespace_id(),
        service_id: service_id(),
        origin: None,
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: EventSequence::try_new(4).expect("sequence"),
    };

    let evidence = status
        .interruption_evidence(OperationInterruptionCause::PriorCoreProcessLoss)
        .expect("worker-owned deploy is recoverable");

    assert_eq!(
        evidence.last_durable_stage(),
        OperationInterruptionStage::Deploy {
            stage: DeployInterruptionStage::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        }
    );
    assert_eq!(
        evidence.uncertain_work(),
        OperationInterruptionUncertainWork::IntentAndRuntime
    );
    assert_eq!(
        evidence.next_action(),
        OperationInterruptionNextAction::RetryFromObservedReality
    );
    let encoded = serde_json::to_value(&evidence).expect("interruption evidence serializes");
    assert_eq!(
        encoded.get("cause").and_then(serde_json::Value::as_str),
        Some("prior_core_process_loss")
    );
    assert_eq!(
        encoded
            .get("last_durable_stage")
            .and_then(|stage| stage.get("kind"))
            .and_then(serde_json::Value::as_str),
        Some("deploy")
    );
    assert_eq!(
        encoded.get("kind").and_then(serde_json::Value::as_str),
        Some("deploy")
    );
    assert_eq!(
        encoded
            .get("uncertain_work")
            .and_then(serde_json::Value::as_str),
        Some("intent_and_runtime")
    );
    assert_eq!(
        encoded
            .get("next_action")
            .and_then(serde_json::Value::as_str),
        Some("retry_from_observed_reality")
    );
    assert_eq!(
        serde_json::from_value::<ployz_core::operation::OperationInterruptionEvidence>(
            encoded.clone(),
        )
        .expect("interruption evidence deserializes"),
        evidence
    );
    for (field, forged) in [
        ("kind", serde_json::json!("cert")),
        ("uncertain_work", serde_json::json!("runtime")),
        ("next_action", serde_json::json!("inspect_then_resubmit")),
    ] {
        let mut forged_evidence = encoded.clone();
        forged_evidence
            .as_object_mut()
            .expect("evidence is an object")
            .insert(field.to_owned(), forged);
        assert!(
            serde_json::from_value::<ployz_core::operation::OperationInterruptionEvidence>(
                forged_evidence,
            )
            .is_err(),
            "forged {field} is rejected"
        );
    }

    let event = OperationEvent::OperationInterrupted {
        operation_id: status.id().clone(),
        evidence: evidence.clone(),
    };
    let projected =
        project_operation_event(&status, event, EventSequence::try_new(5).expect("sequence"))
            .expect("interruption projects");
    let ployz_core::operation::OperationProjection::StatusChanged { status } = projected else {
        panic!("interruption changes status");
    };
    let projected_operation_id = status.id().clone();
    let OperationStatus::Deploy {
        ref id,
        ref namespace_id,
        ref service_id,
        ref origin,
        state: DeployOperationState::Interrupted {
            evidence: ref actual,
        },
        ref last_event_sequence,
    } = *status
    else {
        panic!("deploy becomes interrupted");
    };
    assert_eq!(*actual, evidence);
    assert_eq!(id, &operation_id("op-deploy"));
    assert_eq!(namespace_id, &self::namespace_id());
    assert_eq!(service_id, &self::service_id());
    assert_eq!(origin, &None);
    assert_eq!(last_event_sequence.get(), 5);

    let late_event = OperationEvent::DeployContainerStarted {
        operation_id: projected_operation_id,
        machine_id: ployz_core::ids::MachineId::try_new("edge-one").expect("machine id"),
        container_id: ployz_core::ids::ContainerId::try_new("late-container")
            .expect("container id"),
    };
    let late_projection = project_operation_event(
        &status,
        late_event,
        EventSequence::try_new(6).expect("sequence"),
    );
    assert!(
        matches!(late_projection, Ok(OperationProjection::AlreadySatisfied)),
        "late evidence result: {late_projection:?}"
    );
}

#[test]
fn separately_recovered_operation_is_not_eligible_for_generic_interruption() {
    let accepted = OperationStatus::deploy_accepted(
        operation_id("op-control"),
        namespace_id(),
        service_id(),
        None,
        EventSequence::first(),
    );
    assert!(
        accepted
            .interruption_evidence(OperationInterruptionCause::CoreShutdown)
            .is_some()
    );

    let cert = OperationStatus::cert_accepted(
        operation_id("op-cert"),
        ployz_core::ids::CertId::try_new("cert-one").expect("cert id"),
        EventSequence::first(),
    );
    assert!(
        cert.interruption_evidence(OperationInterruptionCause::PriorCoreProcessLoss)
            .is_none()
    );
}
