use async_nats::jetstream;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot,
    MachineContainerObservationSnapshotError, ManagedContainerObservation,
};
use ployz_core::state::{
    GatewayServingStatus, GatewayStatusObservation, MachinePublicIpObservation,
};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    container_id, machine_id,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[tokio::test]
async fn container_observation_store_writes_latest_container_to_kv_obs() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let running = managed_observation("ctr_123", ContainerRuntimeState::running_unroutable());
    let exited = managed_observation("ctr_123", ContainerRuntimeState::Exited);

    store
        .replace_machine_containers(&machine_snapshot([running]))
        .await
        .expect("first snapshot stores");
    store
        .replace_machine_containers(&machine_snapshot([exited.clone()]))
        .await
        .expect("second snapshot stores");

    assert_eq!(
        store
            .container(&machine_id("machine_7"), &container_id("ctr_123"))
            .await
            .expect("container observation loads"),
        Some(exited)
    );
}

#[tokio::test]
async fn container_observation_snapshot_removes_stale_machine_containers() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let stale = managed_observation("ctr_123", ContainerRuntimeState::running_unroutable());
    let retained = managed_observation("ctr_456", ContainerRuntimeState::running_unroutable());

    store
        .replace_machine_containers(&machine_snapshot([stale, retained.clone()]))
        .await
        .expect("initial machine snapshot stores");
    store
        .replace_machine_containers(&machine_snapshot([retained.clone()]))
        .await
        .expect("machine snapshot stores");

    assert_eq!(
        store
            .container(&machine_id("machine_7"), &container_id("ctr_123"))
            .await
            .expect("stale lookup succeeds"),
        None
    );
    assert_eq!(
        store
            .container(&machine_id("machine_7"), &container_id("ctr_456"))
            .await
            .expect("retained lookup succeeds"),
        Some(retained)
    );
}

#[tokio::test]
async fn machine_observation_snapshots_list_sorted_current_snapshots() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let machine_7 = machine_snapshot_for(
        "machine_7",
        [managed_observation_for(
            "machine_7",
            "ctr_7",
            ContainerRuntimeState::running_unroutable(),
        )],
    );
    let machine_8 = machine_snapshot_for(
        "machine_8",
        [managed_observation_for(
            "machine_8",
            "ctr_8_old",
            ContainerRuntimeState::Exited,
        )],
    );
    let machine_8_current = machine_snapshot_for(
        "machine_8",
        [managed_observation_for(
            "machine_8",
            "ctr_8",
            ContainerRuntimeState::running_unroutable(),
        )],
    );

    let machine_8_store = nats.machine_8().await;
    machine_8_store
        .replace_machine_containers(&machine_8)
        .await
        .expect("old machine 8 snapshot stores");
    store
        .replace_machine_containers(&machine_7)
        .await
        .expect("machine 7 snapshot stores");
    machine_8_store
        .replace_machine_containers(&machine_8_current)
        .await
        .expect("machine 8 snapshot replaces old");

    assert_eq!(
        store.machine_snapshots().await.expect("snapshots list"),
        vec![machine_7, machine_8_current]
    );
}

#[tokio::test]
async fn machine_public_ip_observations_are_latest_per_machine() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let old = machine_public_ip("machine_7", [203, 0, 113, 7]);
    let current = machine_public_ip("machine_7", [203, 0, 113, 8]);
    let other = machine_public_ip("machine_8", [203, 0, 113, 9]);

    store
        .replace_machine_public_ip(&old)
        .await
        .expect("old ip stores");
    nats.machine_8()
        .await
        .replace_machine_public_ip(&other)
        .await
        .expect("other ip stores");
    store
        .replace_machine_public_ip(&current)
        .await
        .expect("current ip stores");

    assert_eq!(
        store
            .machine_public_ip(&machine_id("machine_7"))
            .await
            .expect("public ip loads"),
        Some(current.clone())
    );
    assert_eq!(
        store.machine_public_ips().await.expect("public ips list"),
        vec![current, other]
    );
}

#[tokio::test]
async fn machine_public_ip_observation_clear_removes_stale_value() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let machine_id = machine_id("machine_7");
    let stale = machine_public_ip("machine_7", [203, 0, 113, 7]);

    store
        .replace_machine_public_ip(&stale)
        .await
        .expect("public ip stores");
    store
        .clear_machine_public_ip(&machine_id)
        .await
        .expect("public ip clears");

    assert_eq!(
        store
            .machine_public_ip(&machine_id)
            .await
            .expect("public ip lookup succeeds"),
        None
    );
    assert_eq!(
        store.machine_public_ips().await.expect("public ips list"),
        Vec::<MachinePublicIpObservation>::new()
    );
}

#[tokio::test]
async fn gateway_status_observations_are_latest_per_machine() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let old = gateway_status("machine_7", GatewayServingStatus::Unavailable, 0);
    let current = gateway_status("machine_7", GatewayServingStatus::Current, 2);
    let other = gateway_status("machine_8", GatewayServingStatus::LastKnownGood, 1);

    store
        .replace_gateway_status(&old)
        .await
        .expect("old gateway status stores");
    nats.machine_8()
        .await
        .replace_gateway_status(&other)
        .await
        .expect("other gateway status stores");
    store
        .replace_gateway_status(&current)
        .await
        .expect("current gateway status stores");

    assert_eq!(
        store
            .gateway_status(&machine_id("machine_7"))
            .await
            .expect("gateway status loads"),
        Some(current.clone())
    );
    assert_eq!(
        store
            .gateway_statuses()
            .await
            .expect("gateway statuses list"),
        vec![current, other]
    );
}

#[tokio::test]
async fn container_observation_snapshot_rejects_wrong_machine_before_store_write() {
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

#[tokio::test]
async fn missing_container_observation_returns_none() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");

    assert_eq!(
        store
            .container(&machine_id("machine_7"), &container_id("ctr_missing"))
            .await
            .expect("missing container lookup succeeds"),
        None
    );
}

struct TestNats {
    _server: ployz_test_support::nats::TestNats,
    /// `machine_7`'s Machine principal: each machine may only write its own
    /// observation keys, so cross-machine fixtures need [`TestNats::machine_8`].
    jetstream: jetstream::Context,
    machine_8_jetstream: jetstream::Context,
}

impl TestNats {
    /// The observation store connected as `machine_8` — the only principal
    /// allowed to write `machine_8`'s observation keys.
    async fn machine_8(&self) -> AsyncNatsObservationStore {
        AsyncNatsObservationStore::from_jetstream(&self.machine_8_jetstream)
            .await
            .expect("open machine_8 observation store")
    }
}

/// A secured server where the buckets are bootstrapped by the Controller
/// and the observation stores connect as Machines — the principal that writes
/// observations in production.
async fn test_nats() -> TestNats {
    let server = ployz_test_support::nats::TestNats::start_with_machines(&[
        machine_id("machine_7"),
        machine_id("machine_8"),
    ])
    .await;
    server.bootstrap_resources().await;
    let jetstream = jetstream::new(server.machine_client(&machine_id("machine_7")).await);
    let machine_8_jetstream = jetstream::new(server.machine_client(&machine_id("machine_8")).await);

    TestNats {
        _server: server,
        jetstream,
        machine_8_jetstream,
    }
}

fn managed_observation(
    container_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    managed_observation_for("machine_7", container_id_value, state)
}

fn managed_observation_for(
    machine_id_value: &str,
    container_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    let mut observation = containers::observation(machine_id_value, container_id_value)
        .entry("rev_1")
        .operation("op_123")
        .step("step_1")
        .build();
    observation.state = state;
    observation
}

fn machine_snapshot(
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    machine_snapshot_for("machine_7", containers)
}

fn machine_snapshot_for(
    machine_id_value: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(machine_id_value), containers)
        .expect("matching machine snapshot")
}

fn machine_public_ip(machine_id_value: &str, octets: [u8; 4]) -> MachinePublicIpObservation {
    MachinePublicIpObservation {
        machine_id: machine_id(machine_id_value),
        public_ip: IpAddr::V4(Ipv4Addr::from(octets)),
    }
}

fn gateway_status(
    machine_id_value: &str,
    serving: GatewayServingStatus,
    route_count: usize,
) -> GatewayStatusObservation {
    GatewayStatusObservation {
        machine_id: machine_id(machine_id_value),
        listen_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 8080)),
        serving,
        route_count,
    }
}
