use ployz_core::ids::{ContainerId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::ManagedContainerKind;
use ployzd::docker::labels::ManagedContainerLabels;
use ployzd::node_agent::runtime::{
    ExistingManagedContainer, ExistingManagedContainerState, NodeContainerRunConflict,
    NodeContainerRunDecision, decide_container_run,
};

#[test]
fn matching_operation_step_and_request_labels_reuse_existing_container() {
    let expected = run_labels("op_123", "step_1");

    assert_eq!(
        decide_container_run(
            &expected,
            [existing_container("ctr_existing", expected.clone())]
        ),
        NodeContainerRunDecision::ReuseRunning {
            container_id: container_id("ctr_existing"),
        }
    );
}

#[test]
fn same_operation_step_with_different_request_metadata_conflicts() {
    let expected = run_labels("op_123", "step_1");
    let mut conflicting_labels = expected.clone();
    conflicting_labels.revision_id = revision_id("rev_2");

    assert_eq!(
        decide_container_run(
            &expected,
            [existing_container(
                "ctr_existing",
                conflicting_labels.clone()
            )]
        ),
        NodeContainerRunDecision::Conflict(NodeContainerRunConflict {
            container_id: container_id("ctr_existing"),
            expected,
            actual: conflicting_labels,
        })
    );
}

#[test]
fn different_step_does_not_reuse_container() {
    let expected = run_labels("op_123", "step_1");
    let other_step = run_labels("op_123", "step_2");

    assert_eq!(
        decide_container_run(&expected, [existing_container("ctr_other", other_step)]),
        NodeContainerRunDecision::Create { labels: expected }
    );
}

#[test]
fn stopped_matching_operation_step_starts_existing_container() {
    let expected = run_labels("op_123", "step_1");

    assert_eq!(
        decide_container_run(
            &expected,
            [existing_container_with_state(
                "ctr_existing",
                expected.clone(),
                ExistingManagedContainerState::StartableStopped,
            )]
        ),
        NodeContainerRunDecision::StartExisting {
            container_id: container_id("ctr_existing"),
        }
    );
}

#[test]
fn non_startable_matching_operation_step_reports_not_startable() {
    let expected = run_labels("op_123", "step_1");

    assert_eq!(
        decide_container_run(
            &expected,
            [existing_container_with_state(
                "ctr_existing",
                expected.clone(),
                ExistingManagedContainerState::NotStartable {
                    description: "paused".to_owned(),
                },
            )]
        ),
        NodeContainerRunDecision::NotStartable {
            container_id: container_id("ctr_existing"),
            state: ExistingManagedContainerState::NotStartable {
                description: "paused".to_owned(),
            },
        }
    );
}

#[test]
fn duplicate_operation_step_matches_are_ambiguous() {
    let expected = run_labels("op_123", "step_1");

    assert_eq!(
        decide_container_run(
            &expected,
            [
                existing_container("ctr_a", expected.clone()),
                existing_container("ctr_b", expected.clone()),
            ]
        ),
        NodeContainerRunDecision::Ambiguous {
            operation_id: operation_id("op_123"),
            step_id: step_id("step_1"),
            container_ids: vec![container_id("ctr_a"), container_id("ctr_b")],
        }
    );
}

fn existing_container(
    container_id: &str,
    labels: ManagedContainerLabels,
) -> ExistingManagedContainer {
    existing_container_with_state(container_id, labels, ExistingManagedContainerState::Running)
}

fn existing_container_with_state(
    container_id: &str,
    labels: ManagedContainerLabels,
    state: ExistingManagedContainerState,
) -> ExistingManagedContainer {
    ExistingManagedContainer {
        container_id: self::container_id(container_id),
        labels,
        state,
    }
}

fn run_labels(operation_id: &str, step_id: &str) -> ManagedContainerLabels {
    ManagedContainerLabels {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
        operation_id: self::operation_id(operation_id),
        step_id: self::step_id(step_id),
        kind: ManagedContainerKind::Service,
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}
