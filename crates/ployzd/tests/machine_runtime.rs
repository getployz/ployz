use ployz_core::dataplane::DataplanePrepareRequest;
use ployz_core::deploy::ImageReference;
use ployz_core::machine_runtime::ManagedContainerIdentity;
use ployz_core::machine_runtime::{ContainerRuntimeState, ManagedContainerKind};
use ployz_test_support::containers;
use ployz_test_support::ids::{
    machine_id, namespace_revision_entry_id, operation_id, service_id, step_id,
};
use ployzd::deploy_worker::{DataplanePreparer, MachineContainerRuntime};
use ployzd::roles::machine::client::{NatsMachineContainerRuntime, NatsMachineDataplanePreparer};
use ployzd::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRunRpcRequest, MachineRunContainerOutcome,
};
use ployzd::roles::machine::service::start_machine_runtime_service;

mod support;

use support::machine_runtime::{ObservingContainerRunner, ReadyWireGuardEbpf};

#[tokio::test]
async fn machine_runtime_serves_container_run_and_observes_created_container() {
    let nats = TestNats::start_bootstrapped().await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let runtime = start_machine_runtime_service(
        nats.machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner.clone(),
    )
    .await
    .expect("machine runtime starts");
    let mut container_runtime = NatsMachineContainerRuntime::new(nats.client.clone());

    let first = container_runtime
        .run_container(&machine_id("machine_a"), run_request("run_1"))
        .await
        .expect("container run succeeds");
    let first_container_id = first.container_id().clone();
    assert!(matches!(first, MachineRunContainerOutcome::Created { .. }));
    assert_observed_running(&runner, &first_container_id);

    let second = container_runtime
        .run_container(&machine_id("machine_a"), run_request("run_1"))
        .await
        .expect("duplicate operation step reuses container");
    assert_eq!(
        second,
        MachineRunContainerOutcome::ReusedRunning {
            container_id: first_container_id
        }
    );

    runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
}

#[tokio::test]
async fn machine_runtime_serves_container_remove_and_updates_observations() {
    let nats = TestNats::start_bootstrapped().await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let runtime = start_machine_runtime_service(
        nats.machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner.clone(),
    )
    .await
    .expect("machine runtime starts");
    let mut container_runtime = NatsMachineContainerRuntime::new(nats.client.clone());

    let created = container_runtime
        .run_container(&machine_id("machine_a"), run_request("run_1"))
        .await
        .expect("container run succeeds");
    let container_id = created.container_id().clone();
    assert_observed_running(&runner, &container_id);

    container_runtime
        .remove_container(
            &machine_id("machine_a"),
            MachineContainerRemoveRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id.clone(),
                expected_identity: managed_identity("run_1"),
            },
        )
        .await
        .expect("container remove succeeds");

    assert!(runner.snapshot().container(&container_id).is_none());

    runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
}

#[tokio::test]
async fn machine_runtime_serves_wireguard_ebpf_prepare() {
    let nats = TestNats::start_bootstrapped().await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let runtime = start_machine_runtime_service(
        nats.machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner,
    )
    .await
    .expect("machine runtime starts");
    let mut preparer = NatsMachineDataplanePreparer::new(nats.client.clone());

    preparer
        .prepare_dataplane(DataplanePrepareRequest::for_machines(
            operation_id("op_123"),
            vec![machine_id("machine_a")],
        ))
        .await
        .expect("dataplane prepare succeeds");

    runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the deploy-worker request side.
    client: async_nats::Client,
    /// Machine principal: the machine-runtime service side.
    machine_client: async_nats::Client,
}

impl TestNats {
    async fn start_bootstrapped() -> Self {
        let nats =
            ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")])
                .await;
        let client = nats.controller.clone();
        let machine_client = nats.machine_client(&machine_id("machine_a")).await;

        Self {
            _nats: nats,
            client,
            machine_client,
        }
    }
}

fn assert_observed_running(
    runner: &ObservingContainerRunner,
    container_id: &ployz_core::ids::ContainerId,
) {
    let snapshot = runner.snapshot();
    let observation = snapshot
        .container(container_id)
        .expect("created container is observed");

    assert_eq!(observation.identity.service_id, service_id("svc_api"));
    assert_eq!(
        observation.identity.namespace_revision_entry_id,
        namespace_revision_entry_id("entry_2")
    );
    assert_eq!(observation.identity.operation_id, operation_id("op_123"));
    assert_eq!(observation.identity.step_id, step_id("run_1"));
    assert_eq!(observation.identity.kind, ManagedContainerKind::Service);
    // Every started container joins the endpoint network (ADR 0023), so
    // the observation always carries an endpoint IP.
    assert_eq!(
        observation.state,
        ContainerRuntimeState::running_at(std::net::Ipv4Addr::LOCALHOST.into())
    );
}

fn run_request(step: &str) -> MachineContainerRunRpcRequest {
    MachineContainerRunRpcRequest {
        image: image("ghcr.io/acme/api:rev-2"),
        container: managed_identity(step),
    }
}

fn managed_identity(step: &str) -> ManagedContainerIdentity {
    containers::identity("svc_api")
        .entry("entry_2")
        .operation("op_123")
        .step(step)
        .build()
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}
