use crate::control::operations::deploy::MachineContainerRuntime;
use crate::control::role_client::machine::NatsMachineContainerRuntime;
use crate::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRunRpcRequest, MachineImagePull,
    MachineRunContainerOutcome,
};
use crate::roles::machine::service::start_machine_role_service;
use ployz_core::deploy::ImageReference;
use ployz_core::machine::runtime::ManagedContainerIdentity;
use ployz_core::machine::runtime::{ContainerRuntimeState, ManagedContainerKind};
use ployz_test_support::containers;
use ployz_test_support::ids::{
    machine_id, namespace_revision_entry_id, operation_id, service_id, step_id,
};

use crate::tests::support::machine_runtime::{ObservingContainerRunner, ReadyWireGuardEbpf};

#[tokio::test]
async fn machine_runtime_serves_container_run_and_observes_created_container() {
    let nats = TestNats::start_bootstrapped().await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let runtime = start_machine_role_service(
        nats.machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id("machine_a")),
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
    let runtime = start_machine_role_service(
        nats.machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id("machine_a")),
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
    assert!(matches!(
        &observation.state,
        ContainerRuntimeState::Running {
            ip: Some(ip),
            health: ployz_core::machine::runtime::ContainerHealth::None,
            started_at_unix_ms: Some(_),
        } if *ip == std::net::Ipv4Addr::LOCALHOST
    ));
}

fn run_request(step: &str) -> MachineContainerRunRpcRequest {
    MachineContainerRunRpcRequest {
        pull: MachineImagePull::Registry {
            credential: None,
            reference: image("ghcr.io/acme/api:rev-2"),
        },
        runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
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
