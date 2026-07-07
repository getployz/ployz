use ployz_core::machine_runtime::ManagedContainerIdentity;
use ployz_test_support::containers;
use ployz_test_support::ids::{container_id, operation_id, step_id};
use ployzd::roles::machine::runner::{
    ExistingManagedContainer, ExistingManagedContainerState, MachineContainerRunDecision,
    decide_container_run,
};

#[test]
fn matching_operation_step_and_request_labels_reuse_existing_container() {
    let expected = run_identity("op_123", "step_1");

    assert_eq!(
        decide_container_run(
            &expected,
            [existing_container("ctr_existing", expected.clone())]
        ),
        MachineContainerRunDecision::ReuseRunning {
            container_id: container_id("ctr_existing"),
        }
    );
}

#[test]
fn same_operation_step_with_different_service_creates_container() {
    let expected = run_identity("op_123", "step_1");
    let sibling_service = containers::identity("svc_worker")
        .entry("entry_worker")
        .operation("op_123")
        .step("step_1")
        .build();

    assert_eq!(
        decide_container_run(
            &expected,
            [existing_container("ctr_existing", sibling_service)]
        ),
        MachineContainerRunDecision::Create { identity: expected }
    );
}

#[test]
fn different_step_does_not_reuse_container() {
    let expected = run_identity("op_123", "step_1");
    let other_step = run_identity("op_123", "step_2");

    assert_eq!(
        decide_container_run(&expected, [existing_container("ctr_other", other_step)]),
        MachineContainerRunDecision::Create { identity: expected }
    );
}

#[test]
fn stopped_matching_operation_step_starts_existing_container() {
    let expected = run_identity("op_123", "step_1");

    assert_eq!(
        decide_container_run(
            &expected,
            [existing_container_with_state(
                "ctr_existing",
                expected.clone(),
                ExistingManagedContainerState::StartableStopped,
            )]
        ),
        MachineContainerRunDecision::StartExisting {
            container_id: container_id("ctr_existing"),
        }
    );
}

#[test]
fn non_startable_matching_operation_step_reports_not_startable() {
    let expected = run_identity("op_123", "step_1");

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
        MachineContainerRunDecision::NotStartable {
            container_id: container_id("ctr_existing"),
            state: ExistingManagedContainerState::NotStartable {
                description: "paused".to_owned(),
            },
        }
    );
}

#[test]
fn duplicate_operation_step_matches_are_ambiguous() {
    let expected = run_identity("op_123", "step_1");

    assert_eq!(
        decide_container_run(
            &expected,
            [
                existing_container("ctr_a", expected.clone()),
                existing_container("ctr_b", expected.clone()),
            ]
        ),
        MachineContainerRunDecision::Ambiguous {
            operation_id: operation_id("op_123"),
            step_id: step_id("step_1"),
            container_ids: vec![container_id("ctr_a"), container_id("ctr_b")],
        }
    );
}

fn existing_container(
    container_id: &str,
    identity: ManagedContainerIdentity,
) -> ExistingManagedContainer {
    existing_container_with_state(
        container_id,
        identity,
        ExistingManagedContainerState::Running { ip: None },
    )
}

fn existing_container_with_state(
    container_id: &str,
    identity: ManagedContainerIdentity,
    state: ExistingManagedContainerState,
) -> ExistingManagedContainer {
    ExistingManagedContainer {
        container_id: self::container_id(container_id),
        identity,
        state,
    }
}

fn run_identity(operation_id: &str, step_id: &str) -> ManagedContainerIdentity {
    containers::identity("svc_api")
        .entry("entry_1")
        .operation(operation_id)
        .step(step_id)
        .build()
}
