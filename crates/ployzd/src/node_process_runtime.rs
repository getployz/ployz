//! Runtime wiring for the node role.

use crate::config::NodeProcessConfig;
use crate::dataplane_runtime::HostWireGuardEbpfPreparer;
use crate::docker::runner::LazyLocalDockerManagedContainerRunner;
use crate::node_agent::runtime::{
    ExistingManagedContainerState, NodeContainerRunner, NodeContainerRunnerError, NodeLogReader,
};
use crate::node_runtime::{
    NodeRuntimeStartError, RunningNodeRuntime, start_node_runtime_with_ports,
};
use ployz_core::ids::NodeId;
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerObservation, NodeContainerObservationSnapshot,
    NodeContainerObservationSnapshotError,
};
use ployz_core::state::NodePublicIpObservation;
use ployz_nats::connect::{NatsConnectError, connect_with_timeout};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError};
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const NODE_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const NODE_OBSERVATION_INTERVAL: Duration = Duration::from_secs(1);
const NODE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RunningNodeProcessRuntime {
    node_service: RunningNodeRuntime,
    observer: RunningNodeObserverRuntime,
}

impl RunningNodeProcessRuntime {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.observer.shutdown().await;
        self.node_service.shutdown().await
    }

    #[must_use]
    pub fn observer_health(&self) -> NodeObserverHealth {
        self.observer.health()
    }
}

pub async fn start_node_process_runtime(
    config: &NodeProcessConfig,
) -> Result<RunningNodeProcessRuntime, NodeProcessRuntimeError> {
    let client = connect_with_timeout(&config.nats_url, NODE_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(NodeProcessRuntimeError::ConnectNats)?;
    let runner =
        LazyLocalDockerManagedContainerRunner::new(config.dataplane_endpoint_subnet.clone());
    let preparer = HostWireGuardEbpfPreparer::new(
        config.node_id.clone(),
        config.ebpf_bytecode_path.clone(),
        config.ebpf_ctl_path.clone(),
        config.dataplane_bridge_ifname.clone(),
        config.dataplane_wg_ifname.clone(),
    );

    start_node_process_runtime_with_ports(
        client,
        config.node_id.clone(),
        runner.clone(),
        preparer,
        runner,
        config.public_ip,
        NODE_OBSERVATION_INTERVAL,
    )
    .await
}

pub async fn start_node_process_runtime_with_ports<R, P, L>(
    client: NatsClient,
    node_id: NodeId,
    runner: R,
    preparer: P,
    log_reader: L,
    public_ip: Option<IpAddr>,
    observation_interval: Duration,
) -> Result<RunningNodeProcessRuntime, NodeProcessRuntimeError>
where
    R: Clone + NodeContainerRunner + Send + Sync + 'static,
    P: Clone + crate::node_service_runtime::NodeWireGuardEbpfPreparer + Send + Sync + 'static,
    L: Clone + NodeLogReader + Send + Sync + 'static,
{
    let node_service = start_node_runtime_with_ports(
        client.clone(),
        node_id.clone(),
        runner.clone(),
        preparer,
        log_reader,
    )
    .await
    .map_err(NodeProcessRuntimeError::StartNodeService)?;
    let observer =
        start_node_observer_runtime(node_id, runner, client, public_ip, observation_interval);

    Ok(RunningNodeProcessRuntime {
        node_service,
        observer,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeObserverHealth {
    pub last_attempt: Option<NodeObserverAttempt>,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeObserverAttempt {
    Succeeded,
    Failed { message: String },
}

struct RunningNodeObserverRuntime {
    health: Arc<Mutex<NodeObserverHealth>>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningNodeObserverRuntime {
    fn health(&self) -> NodeObserverHealth {
        self.health
            .lock()
            .expect("node observer health lock is not poisoned")
            .clone()
    }

    async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

fn start_node_observer_runtime<R>(
    node_id: NodeId,
    runner: R,
    client: NatsClient,
    public_ip: Option<IpAddr>,
    interval: Duration,
) -> RunningNodeObserverRuntime
where
    R: NodeContainerRunner + Send + Sync + 'static,
{
    let health = Arc::new(Mutex::new(NodeObserverHealth {
        last_attempt: None,
        consecutive_failures: 0,
    }));
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let task_health = Arc::clone(&health);
    let task = tokio::spawn(async move {
        let mut backoff = interval;
        let mut publisher = NodeObservationPublisher::new(client);
        loop {
            let attempt = publisher
                .publish_with_timeout(&node_id, &runner, public_ip, NODE_OBSERVATION_TIMEOUT)
                .await;
            backoff = record_observer_attempt(&task_health, attempt, interval, backoff);
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = &mut shutdown_rx => break,
            }
        }
    });

    RunningNodeObserverRuntime {
        health,
        shutdown,
        task,
    }
}

fn record_observer_attempt(
    health: &Mutex<NodeObserverHealth>,
    attempt: Result<(), NodeProcessRuntimeError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health
        .lock()
        .expect("node observer health lock is not poisoned");
    match attempt {
        Ok(()) => {
            health.last_attempt = Some(NodeObserverAttempt::Succeeded);
            health.consecutive_failures = 0;
            interval
        }
        Err(error) => {
            health.last_attempt = Some(NodeObserverAttempt::Failed {
                message: error.to_string(),
            });
            health.consecutive_failures += 1;
            current_backoff
                .saturating_mul(2)
                .min(Duration::from_secs(30))
        }
    }
}

struct NodeObservationPublisher {
    client: NatsClient,
    observations: Option<AsyncNatsObservationStore>,
}

impl NodeObservationPublisher {
    fn new(client: NatsClient) -> Self {
        Self {
            client,
            observations: None,
        }
    }

    async fn publish_with_timeout<R>(
        &mut self,
        node_id: &NodeId,
        runner: &R,
        public_ip: Option<IpAddr>,
        timeout: Duration,
    ) -> Result<(), NodeProcessRuntimeError>
    where
        R: NodeContainerRunner,
    {
        tokio::time::timeout(timeout, self.publish(node_id, runner, public_ip))
            .await
            .map_err(|_| NodeProcessRuntimeError::ObservationTimedOut { timeout })?
    }

    async fn publish<R>(
        &mut self,
        node_id: &NodeId,
        runner: &R,
        public_ip: Option<IpAddr>,
    ) -> Result<(), NodeProcessRuntimeError>
    where
        R: NodeContainerRunner,
    {
        if self.observations.is_none() {
            let jetstream = async_nats::jetstream::new(self.client.clone());
            self.observations = Some(
                AsyncNatsObservationStore::from_jetstream(&jetstream)
                    .await
                    .map_err(NodeProcessRuntimeError::OpenObservations)?,
            );
        }

        publish_node_observation_snapshot(
            node_id,
            runner,
            self.observations
                .as_ref()
                .expect("observation store is opened before publish"),
        )
        .await?;

        let observations = self
            .observations
            .as_ref()
            .expect("observation store is opened before publish");
        match public_ip {
            Some(public_ip) => {
                observations
                    .replace_node_public_ip(&NodePublicIpObservation {
                        node_id: node_id.clone(),
                        public_ip,
                    })
                    .await
            }
            None => observations.clear_node_public_ip(node_id).await,
        }
        .map_err(NodeProcessRuntimeError::PublishObservation)?;

        Ok(())
    }
}

async fn publish_node_observation_snapshot<R>(
    node_id: &NodeId,
    runner: &R,
    observations: &AsyncNatsObservationStore,
) -> Result<(), NodeProcessRuntimeError>
where
    R: NodeContainerRunner,
{
    let existing = runner
        .existing_managed_containers()
        .await
        .map_err(NodeProcessRuntimeError::ListContainers)?;
    let containers = existing
        .into_iter()
        .map(|container| ManagedContainerObservation {
            node_id: node_id.clone(),
            container_id: container.container_id,
            service_id: container.labels.service_id,
            revision_id: container.labels.revision_id,
            operation_id: container.labels.operation_id,
            step_id: container.labels.step_id,
            kind: container.labels.kind,
            state: observation_state(container.state),
        });
    let snapshot = NodeContainerObservationSnapshot::try_new(node_id.clone(), containers)
        .map_err(NodeProcessRuntimeError::BuildSnapshot)?;
    observations
        .replace_node_containers(&snapshot)
        .await
        .map_err(NodeProcessRuntimeError::PublishObservation)
}

fn observation_state(state: ExistingManagedContainerState) -> ContainerRuntimeState {
    match state {
        ExistingManagedContainerState::Running { endpoint } => {
            ContainerRuntimeState::Running { endpoint }
        }
        ExistingManagedContainerState::StartableStopped
        | ExistingManagedContainerState::NotStartable { .. } => ContainerRuntimeState::Exited,
    }
}

pub async fn run_node_until_shutdown(
    config: &NodeProcessConfig,
) -> Result<(), NodeProcessRuntimeError> {
    let runtime = start_node_process_runtime(config).await?;
    wait_for_shutdown_signal()
        .await
        .map_err(NodeProcessRuntimeError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(NodeProcessRuntimeError::ShutdownNodeService)
}

async fn wait_for_shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

#[derive(Debug)]
pub enum NodeProcessRuntimeError {
    ConnectNats(NatsConnectError),
    OpenObservations(ObservationStoreError),
    ListContainers(NodeContainerRunnerError),
    BuildSnapshot(NodeContainerObservationSnapshotError),
    PublishObservation(ObservationStoreError),
    ObservationTimedOut { timeout: Duration },
    StartNodeService(NodeRuntimeStartError),
    ShutdownSignal(std::io::Error),
    ShutdownNodeService(NatsServiceShutdownError),
}

impl fmt::Display for NodeProcessRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::OpenObservations(error) => {
                write!(formatter, "failed to open observation store: {error}")
            }
            Self::ListContainers(error) => {
                write!(
                    formatter,
                    "failed to list managed Docker containers: {error:?}"
                )
            }
            Self::BuildSnapshot(error) => {
                write!(formatter, "failed to build node snapshot: {error}")
            }
            Self::PublishObservation(error) => {
                write!(formatter, "failed to publish node observation: {error}")
            }
            Self::ObservationTimedOut { timeout } => {
                write!(
                    formatter,
                    "node observation publish timed out after {}s",
                    timeout.as_secs()
                )
            }
            Self::StartNodeService(error) => write!(formatter, "{error}"),
            Self::ShutdownSignal(error) => {
                write!(formatter, "failed to wait for shutdown: {error}")
            }
            Self::ShutdownNodeService(error) => {
                write!(formatter, "failed to stop node service: {error:?}")
            }
        }
    }
}

impl std::error::Error for NodeProcessRuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::labels::ManagedContainerIdentity;
    use crate::node_agent::runtime::{
        CreateManagedContainer, ExistingManagedContainer, NodeContainerRunner,
        NodeContainerRunnerError, NodeLogReader, NodeLogReaderError, NodeLogTail,
    };
    use ployz_core::dataplane::{
        EbpfForwardingReady, EbpfForwardingReadyEvidence, WireGuardEbpfPrepareError,
        WireGuardEbpfReady, WireGuardReady, WireGuardReadyEvidence,
    };
    use ployz_core::ids::{ContainerId, OperationId, RevisionId, ServiceId, StepId};
    use ployz_core::node::ManagedContainerKind;
    use std::sync::{Arc, Mutex};

    #[test]
    fn stopped_and_not_startable_containers_are_observed_as_exited() {
        assert_eq!(
            observation_state(ExistingManagedContainerState::StartableStopped),
            ContainerRuntimeState::Exited
        );
        assert_eq!(
            observation_state(ExistingManagedContainerState::NotStartable {
                description: "paused".to_owned(),
            }),
            ContainerRuntimeState::Exited
        );
    }

    #[tokio::test]
    async fn node_process_runtime_starts_service_before_observations_are_ready() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = FailingListRunner;

        let runtime = start_node_process_runtime_with_ports(
            nats.client.clone(),
            node_id("node_a"),
            runner.clone(),
            ReadyWireGuardEbpf,
            runner,
            None,
            Duration::from_secs(60),
        )
        .await
        .expect("runtime starts");

        runtime.shutdown().await.expect("runtime shuts down");
    }

    #[derive(Debug, Clone)]
    struct StaticRunner {
        containers: Arc<Mutex<Vec<ExistingManagedContainer>>>,
    }

    impl StaticRunner {
        fn new(containers: impl IntoIterator<Item = ExistingManagedContainer>) -> Self {
            Self {
                containers: Arc::new(Mutex::new(containers.into_iter().collect())),
            }
        }
    }

    impl NodeContainerRunner for StaticRunner {
        async fn existing_managed_containers(
            &self,
        ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
            self.containers
                .lock()
                .map(|containers| containers.clone())
                .map_err(|error| NodeContainerRunnerError::ListExisting {
                    message: error.to_string(),
                })
        }

        async fn create_managed_container(
            &self,
            _command: CreateManagedContainer,
        ) -> Result<ContainerId, NodeContainerRunnerError> {
            Err(NodeContainerRunnerError::Create {
                message: "not used".to_owned(),
            })
        }

        async fn start_managed_container(
            &self,
            container_id: &ContainerId,
        ) -> Result<(), NodeContainerRunnerError> {
            Err(NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }

        async fn remove_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), NodeContainerRunnerError> {
            Err(NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }
    }

    #[derive(Debug, Clone)]
    struct FailingListRunner;

    impl NodeContainerRunner for FailingListRunner {
        async fn existing_managed_containers(
            &self,
        ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
            Err(NodeContainerRunnerError::ListExisting {
                message: "docker unavailable".to_owned(),
            })
        }

        async fn create_managed_container(
            &self,
            _command: CreateManagedContainer,
        ) -> Result<ContainerId, NodeContainerRunnerError> {
            Err(NodeContainerRunnerError::Create {
                message: "not used".to_owned(),
            })
        }

        async fn start_managed_container(
            &self,
            container_id: &ContainerId,
        ) -> Result<(), NodeContainerRunnerError> {
            Err(NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }

        async fn remove_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), NodeContainerRunnerError> {
            Err(NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }
    }

    impl NodeLogReader for FailingListRunner {
        async fn tail_container_logs(
            &self,
            container_id: &ContainerId,
            _tail_lines: Option<u16>,
        ) -> Result<NodeLogTail, NodeLogReaderError> {
            Err(NodeLogReaderError::ReadFailed {
                container_id: container_id.clone(),
                message: "docker unavailable".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn node_observation_publish_records_snapshot_when_docker_lists() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = StaticRunner::new([ExistingManagedContainer {
            container_id: container_id("ctr_123"),
            labels: labels("run_1"),
            state: ExistingManagedContainerState::Running { endpoint: None },
        }]);

        let mut publisher = NodeObservationPublisher::new(nats.client.clone());
        publisher
            .publish_with_timeout(&node_id("node_a"), &runner, None, Duration::from_secs(1))
            .await
            .expect("snapshot publishes");

        let snapshot = nats
            .observations
            .node_snapshot(&node_id("node_a"))
            .await
            .expect("snapshot reads")
            .expect("snapshot exists");
        assert_eq!(snapshot.containers().len(), 1);
        assert_eq!(
            snapshot
                .container(&container_id("ctr_123"))
                .expect("container exists")
                .state,
            ContainerRuntimeState::running_unroutable()
        );
    }

    #[tokio::test]
    async fn node_observation_publish_records_configured_public_ip() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = StaticRunner::new([]);

        let mut publisher = NodeObservationPublisher::new(nats.client.clone());
        publisher
            .publish_with_timeout(
                &node_id("node_a"),
                &runner,
                Some("203.0.113.7".parse::<IpAddr>().expect("valid public ip")),
                Duration::from_secs(1),
            )
            .await
            .expect("snapshot publishes");

        assert_eq!(
            nats.observations
                .node_public_ip(&node_id("node_a"))
                .await
                .expect("public ip reads")
                .expect("public ip exists")
                .public_ip,
            "203.0.113.7".parse::<IpAddr>().expect("valid public ip")
        );
    }

    #[tokio::test]
    async fn node_observation_publish_clears_public_ip_when_unset() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = StaticRunner::new([]);
        let mut publisher = NodeObservationPublisher::new(nats.client.clone());
        let node_id = node_id("node_a");

        publisher
            .publish_with_timeout(
                &node_id,
                &runner,
                Some("203.0.113.7".parse::<IpAddr>().expect("valid public ip")),
                Duration::from_secs(1),
            )
            .await
            .expect("public ip publishes");
        publisher
            .publish_with_timeout(&node_id, &runner, None, Duration::from_secs(1))
            .await
            .expect("public ip clears");

        assert_eq!(
            nats.observations
                .node_public_ip(&node_id)
                .await
                .expect("public ip reads"),
            None
        );
    }

    #[derive(Clone)]
    struct ReadyWireGuardEbpf;

    impl crate::node_service_runtime::NodeWireGuardEbpfPreparer for ReadyWireGuardEbpf {
        async fn prepare_wireguard_ebpf(
            &self,
        ) -> Result<WireGuardEbpfReady, WireGuardEbpfPrepareError> {
            Ok(WireGuardEbpfReady {
                wireguard: WireGuardReady {
                    evidence: vec![WireGuardReadyEvidence::Command {
                        program: "wg".to_owned(),
                        args: vec!["--version".to_owned()],
                    }],
                },
                ebpf_forwarding: EbpfForwardingReady {
                    evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                        path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                        symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
                    }],
                },
            })
        }
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
            let jetstream = async_nats::jetstream::new(client.clone());
            let plan = ployz_nats::bootstrap::BootstrapPlan::for_single_server_client(&client)
                .expect("bootstrap plan builds");
            ployz_nats::bootstrap::assure_nats_resources(&jetstream, &plan)
                .await
                .expect("nats resources are bootstrapped");
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

    fn labels(step: &str) -> crate::docker::labels::ManagedContainerLabels {
        crate::docker::labels::ManagedContainerLabels {
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

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
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
}
