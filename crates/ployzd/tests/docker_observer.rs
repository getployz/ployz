use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot, NodeContainerObservationSnapshotError,
};
use ployz_core::state::NodeContainerObservationKey;
use ployzd::docker::labels::{
    CONTAINER_TYPE_LABEL, MANAGED_LABEL, ManagedContainerLabelError, ManagedContainerLabels,
    OPERATION_ID_LABEL, REVISION_LABEL, SERVICE_ID_LABEL, STEP_ID_LABEL,
};
use ployzd::node_agent::observer::InMemoryObservationStore;

#[test]
fn docker_event_updates_latest_container_observation() {
    let mut store = InMemoryObservationStore::new();
    let observation = managed_observation("ctr_123", ContainerRuntimeState::Running);

    store
        .put_container(observation.clone())
        .expect("container observation stores");

    assert_eq!(
        store.container(&node_id("node_7"), &container_id("ctr_123")),
        Some(&observation)
    );
}

#[test]
fn full_sync_corrects_missed_docker_event() {
    let mut store = InMemoryObservationStore::new();
    store
        .put_container(managed_observation(
            "ctr_123",
            ContainerRuntimeState::Running,
        ))
        .expect("container observation stores");
    let exited = managed_observation("ctr_123", ContainerRuntimeState::Exited);

    store.replace_node_containers(node_snapshot([exited.clone()]));

    assert_eq!(
        store.container(&node_id("node_7"), &container_id("ctr_123")),
        Some(&exited)
    );
}

#[test]
fn full_sync_removes_stale_containers_for_the_node() {
    let mut store = InMemoryObservationStore::new();
    store
        .put_container(managed_observation(
            "ctr_123",
            ContainerRuntimeState::Running,
        ))
        .expect("container observation stores");
    let retained = managed_observation("ctr_456", ContainerRuntimeState::Running);
    store
        .put_container(retained.clone())
        .expect("container observation stores");

    store.replace_node_containers(node_snapshot([retained.clone()]));

    assert_eq!(
        store.container(&node_id("node_7"), &container_id("ctr_123")),
        None
    );
    assert_eq!(
        store.container(&node_id("node_7"), &container_id("ctr_456")),
        Some(&retained)
    );
}

#[test]
fn node_snapshot_rejects_observations_for_a_different_node() {
    let mut wrong_node = managed_observation("ctr_456", ContainerRuntimeState::Running);
    wrong_node.node_id = node_id("node_8");

    assert_eq!(
        NodeContainerObservationSnapshot::try_new(node_id("node_7"), [wrong_node]),
        Err(NodeContainerObservationSnapshotError::NodeMismatch {
            expected: node_id("node_7"),
            actual: node_id("node_8"),
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
        labels.get(REVISION_LABEL).map(String::as_str),
        Some("rev_1")
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
        NodeContainerObservationKey::from_node_id(&node_id("node_7")).as_str(),
        "containers.node_7"
    );
}

fn node_snapshot(
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> NodeContainerObservationSnapshot {
    NodeContainerObservationSnapshot::try_new(node_id("node_7"), containers)
        .expect("matching node snapshot")
}

fn managed_observation(
    container_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id("node_7"),
        container_id: container_id(container_id_value),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
        state,
    }
}

fn managed_labels() -> ManagedContainerLabels {
    ManagedContainerLabels {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
    }
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}
