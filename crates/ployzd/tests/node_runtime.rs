use async_nats::jetstream;
use ployz_core::dataplane::WireGuardEbpfPrepareRequest;
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{ContainerRuntimeState, ManagedContainerKind};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployzd::deploy_worker::{NodeContainerRuntime, WireGuardEbpfPreparer};
use ployzd::docker::labels::ManagedContainerLabels;
use ployzd::node_rpc::{NatsNodeContainerRuntime, NatsNodeWireGuardEbpfPreparer};
use ployzd::node_runtime::start_node_runtime_with_ports;
use ployzd::node_runtime_types::{
    NodeContainerRunSpec, NodeRemoveContainerRequest, NodeRunContainerOutcome,
    NodeRunContainerRequest,
};

mod support;

#[tokio::test]
async fn node_runtime_serves_container_run_and_observes_created_container() {
    let nats = TestNats::start_bootstrapped().await;
    let runtime = start_node_runtime_with_ports(
        nats.client.clone(),
        node_id("node_a"),
        support::ObservingContainerRunner::new(node_id("node_a"), nats.observations.clone()),
        support::ReadyWireGuardEbpf,
        support::ObservingContainerRunner::new(node_id("node_a"), nats.observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let mut container_runtime = NatsNodeContainerRuntime::new(nats.client.clone());

    let first = container_runtime
        .run_container(run_request("run_1"))
        .await
        .expect("container run succeeds");
    let first_container_id = first.container_id().clone();
    assert!(matches!(first, NodeRunContainerOutcome::Created { .. }));
    assert_observed_running(&nats.observations, &first_container_id).await;

    let second = container_runtime
        .run_container(run_request("run_1"))
        .await
        .expect("duplicate operation step reuses container");
    assert_eq!(
        second,
        NodeRunContainerOutcome::ReusedRunning {
            container_id: first_container_id
        }
    );

    runtime.shutdown().await.expect("node runtime shuts down");
}

#[tokio::test]
async fn node_runtime_serves_container_remove_and_updates_observations() {
    let nats = TestNats::start_bootstrapped().await;
    let runtime = start_node_runtime_with_ports(
        nats.client.clone(),
        node_id("node_a"),
        support::ObservingContainerRunner::new(node_id("node_a"), nats.observations.clone()),
        support::ReadyWireGuardEbpf,
        support::ObservingContainerRunner::new(node_id("node_a"), nats.observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let mut container_runtime = NatsNodeContainerRuntime::new(nats.client.clone());

    let created = container_runtime
        .run_container(run_request("run_1"))
        .await
        .expect("container run succeeds");
    let container_id = created.container_id().clone();
    assert_observed_running(&nats.observations, &container_id).await;

    container_runtime
        .remove_container(NodeRemoveContainerRequest {
            node_id: node_id("node_a"),
            operation_id: operation_id("op_123"),
            container_id: container_id.clone(),
            expected_identity: managed_labels("run_1").identity(),
        })
        .await
        .expect("container remove succeeds");

    assert!(
        nats.observations
            .container(&node_id("node_a"), &container_id)
            .await
            .expect("observation reads")
            .is_none()
    );

    runtime.shutdown().await.expect("node runtime shuts down");
}

#[tokio::test]
async fn node_runtime_serves_wireguard_ebpf_prepare() {
    let nats = TestNats::start_bootstrapped().await;
    let runtime = start_node_runtime_with_ports(
        nats.client.clone(),
        node_id("node_a"),
        support::ObservingContainerRunner::new(node_id("node_a"), nats.observations.clone()),
        support::ReadyWireGuardEbpf,
        support::ObservingContainerRunner::new(node_id("node_a"), nats.observations.clone()),
    )
    .await
    .expect("node runtime starts");
    let mut preparer = NatsNodeWireGuardEbpfPreparer::new(nats.client.clone());

    preparer
        .prepare_wireguard_ebpf(WireGuardEbpfPrepareRequest {
            operation_id: operation_id("op_123"),
            nodes: vec![node_id("node_a")],
            endpoint_routes: vec![
                ployz_core::dataplane::WireGuardEbpfEndpointRoute::default_for_node(&node_id(
                    "node_a",
                )),
            ],
        })
        .await
        .expect("wireguard ebpf prepare succeeds");

    runtime.shutdown().await.expect("node runtime shuts down");
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
    observations: AsyncNatsObservationStore,
}

impl TestNats {
    async fn start_bootstrapped() -> Self {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let client = async_nats::connect(server.client_url())
            .await
            .expect("connect to test nats");
        let jetstream = jetstream::new(client.clone());
        bootstrap_nats_resources(&client, &jetstream).await;
        let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
            .await
            .expect("open observations");

        Self {
            _server: server,
            client,
            observations,
        }
    }
}

async fn bootstrap_nats_resources(client: &async_nats::Client, jetstream: &jetstream::Context) {
    let plan = ployz_nats::bootstrap::BootstrapPlan::for_single_server_client(client)
        .expect("bootstrap plan builds");
    ployz_nats::bootstrap::assure_nats_resources(jetstream, &plan)
        .await
        .expect("nats resources are bootstrapped");
}

async fn assert_observed_running(
    observations: &AsyncNatsObservationStore,
    container_id: &ContainerId,
) {
    let observation = observations
        .container(&node_id("node_a"), container_id)
        .await
        .expect("observation reads")
        .expect("created container is observed");

    assert_eq!(observation.service_id, service_id("svc_api"));
    assert_eq!(observation.revision_id, revision_id("rev_2"));
    assert_eq!(observation.operation_id, operation_id("op_123"));
    assert_eq!(observation.step_id, step_id("run_1"));
    assert_eq!(observation.kind, ManagedContainerKind::Service);
    assert_eq!(
        observation.state,
        ContainerRuntimeState::running_unroutable()
    );
}

fn run_request(step: &str) -> NodeRunContainerRequest {
    NodeRunContainerRequest {
        node_id: node_id("node_a"),
        image: image("ghcr.io/acme/api:rev-2"),
        endpoint: None,
        container: NodeContainerRunSpec {
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_2"),
            operation_id: operation_id("op_123"),
            step_id: step_id(step),
            kind: ManagedContainerKind::Service,
        },
    }
}

fn managed_labels(step: &str) -> ManagedContainerLabels {
    ManagedContainerLabels {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id(step),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}
