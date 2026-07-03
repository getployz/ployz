use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    MachineContainerObservationSnapshotError, ManagedContainerKind, ManagedContainerObservation,
};
use ployz_core::state::MachineContainerObservationKey;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id, service_id,
    step_id,
};
use ployzd::docker::labels::{
    CONTAINER_TYPE_LABEL, MANAGED_LABEL, ManagedContainerLabelError, ManagedContainerLabels,
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
    let labels = managed_labels().render();

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
    let labels = managed_labels().render();

    assert_eq!(ManagedContainerLabels::parse(&labels), Ok(managed_labels()));
}

#[test]
fn managed_container_labels_reject_missing_runtime_boundary_data() {
    let mut labels = managed_labels().render();
    labels.remove(STEP_ID_LABEL);

    assert_eq!(
        ManagedContainerLabels::parse(&labels),
        Err(ManagedContainerLabelError::Missing {
            label: STEP_ID_LABEL
        })
    );
}

#[test]
fn managed_container_labels_reject_unknown_container_kind() {
    let mut labels = managed_labels().render();
    labels.insert(CONTAINER_TYPE_LABEL.to_owned(), "sidequest".to_owned());

    assert_eq!(
        ManagedContainerLabels::parse(&labels),
        Err(ManagedContainerLabelError::InvalidKind {
            value: "sidequest".to_owned()
        })
    );
}

#[test]
fn observation_key_matches_kv_obs_container_path() {
    assert_eq!(
        MachineContainerObservationKey::from_machine_id(&machine_id("machine_7")).as_str(),
        "containers.machine_7"
    );
}

fn managed_observation(
    container_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: machine_id("machine_7"),
        container_id: container_id(container_id_value),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
        state,
    }
}

fn managed_labels() -> ManagedContainerLabels {
    ManagedContainerLabels {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
    }
}
