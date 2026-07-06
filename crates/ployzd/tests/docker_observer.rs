use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    MachineContainerObservationSnapshotError, ManagedContainerIdentity,
    ManagedContainerObservation,
};
use ployz_test_support::containers;
use ployz_test_support::ids::{container_id, machine_id};
use ployzd::adapters::docker::labels::{
    self, CONTAINER_TYPE_LABEL, MANAGED_LABEL, ManagedContainerLabelError,
    NAMESPACE_REVISION_ENTRY_LABEL, OPERATION_ID_LABEL, SERVICE_ID_LABEL, STEP_ID_LABEL,
};

#[test]
fn machine_snapshot_rejects_observations_for_a_different_machine() {
    let mut wrong_machine =
        managed_observation("ctr_456", ContainerRuntimeState::running_unroutable());
    wrong_machine.machine_id = machine_id("machine_8");

    assert_eq!(
        MachineContainerObservationSnapshot::try_new(machine_id("machine_7"), [wrong_machine]),
        Err(MachineContainerObservationSnapshotError::MachineMismatch {
            expected: machine_id("machine_7"),
            actual: machine_id("machine_8"),
            container_id: container_id("ctr_456")
        })
    );
}

#[test]
fn managed_containers_render_required_ployz_labels() {
    let labels = labels::render(&managed_identity());

    assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
    assert_eq!(
        labels.get(SERVICE_ID_LABEL).map(String::as_str),
        Some("svc_api")
    );
    assert_eq!(
        labels
            .get(NAMESPACE_REVISION_ENTRY_LABEL)
            .map(String::as_str),
        Some("entry_1")
    );
    assert_eq!(
        labels.get(OPERATION_ID_LABEL).map(String::as_str),
        Some("op_123")
    );
    assert_eq!(
        labels.get(STEP_ID_LABEL).map(String::as_str),
        Some("step_1")
    );
    assert_eq!(
        labels.get(CONTAINER_TYPE_LABEL).map(String::as_str),
        Some("service")
    );
}

#[test]
fn managed_containers_parse_raw_docker_labels() {
    let labels = labels::render(&managed_identity());

    assert_eq!(labels::parse(&labels), Ok(managed_identity()));
}

#[test]
fn managed_container_labels_reject_missing_runtime_boundary_data() {
    let mut labels = labels::render(&managed_identity());
    labels.remove(STEP_ID_LABEL);

    assert_eq!(
        labels::parse(&labels),
        Err(ManagedContainerLabelError::Missing {
            label: STEP_ID_LABEL
        })
    );
}

#[test]
fn managed_container_labels_reject_unknown_container_kind() {
    let mut labels = labels::render(&managed_identity());
    labels.insert(CONTAINER_TYPE_LABEL.to_owned(), "sidequest".to_owned());

    assert_eq!(
        labels::parse(&labels),
        Err(ManagedContainerLabelError::InvalidKind {
            value: "sidequest".to_owned()
        })
    );
}

fn managed_observation(
    container_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    containers::observation("machine_7", container_id_value)
        .with(
            containers::identity("svc_api")
                .entry("entry_1")
                .operation("op_123")
                .step("step_1"),
        )
        .state(state)
        .build()
}

fn managed_identity() -> ManagedContainerIdentity {
    containers::identity("svc_api")
        .entry("entry_1")
        .operation("op_123")
        .step("step_1")
        .build()
}
