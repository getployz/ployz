//! Runtime wiring for the machine role.

use crate::config::MachineProcessConfig;
use crate::dataplane_runtime::{PloyzNativeMeshHostConfig, PloyzNativeMeshPreparer};
use crate::docker::runner::DockerManagedContainerRunner;
use crate::machine_credentials::{AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials};
use crate::machine_runtime::runner::{MachineContainerRunner, MachineLogReader};
use crate::machine_runtime::service::{
    MachineFactsReadError, MachineServiceRuntimeError, current_unix_ms,
    read_machine_facts_snapshot, start_machine_runtime_service_with_public_ip,
};
use crate::process_support::{
    BackoffSchedule, LazyHandle, RecordedAttempt, record_attempt, shutdown_signal,
};
use ployz_core::ids::MachineId;
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError, RunningNatsService};
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const MACHINE_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MACHINE_OBSERVATION_INTERVAL: Duration =
    ployz_core::machine_runtime::OBSERVATION_PUBLISH_INTERVAL;
const MACHINE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RunningMachineProcessRuntime {
    machine_service: RunningNatsService,
    observer: RunningMachineObserverRuntime,
}

impl RunningMachineProcessRuntime {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.observer.shutdown().await;
        self.machine_service.shutdown().await
    }
}

pub async fn start_machine_process_runtime(
    config: &MachineProcessConfig,
) -> Result<RunningMachineProcessRuntime, MachineProcessRuntimeError> {
    // The machine credential may not exist yet (first machine before
    // activate-first-machine): awaiting it is a typed bounded-retry state,
    // not a crash loop.
    let connect = await_role_credentials(
        "machine",
        &config.nats,
        &SeedFileRetryPolicy::default_policy(),
    )
    .await
    .map_err(MachineProcessRuntimeError::AwaitCredentials)?;
    let client = connect_authenticated(&connect, MACHINE_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(MachineProcessRuntimeError::ConnectNats)?;
    let runner = DockerManagedContainerRunner::lazy_local_defaults(
        config.ployz_native_mesh.endpoint_subnet.clone(),
        config.ployz_native_mesh.bridge_ifname.clone(),
    );
    let preparer =
        PloyzNativeMeshPreparer::new(PloyzNativeMeshHostConfig::with_default_key_material(
            config.machine_id.clone(),
            config.artifacts.ebpf_bytecode_path.clone(),
            config.artifacts.ebpf_ctl_path.clone(),
            config.ployz_native_mesh.bridge_ifname.clone(),
            config.ployz_native_mesh.wg_ifname.clone(),
        ));

    start_machine_process_runtime_with_ports(
        client,
        config.machine_id.clone(),
        runner.clone(),
        preparer,
        runner,
        config.public_ip,
        MACHINE_OBSERVATION_INTERVAL,
    )
    .await
}

pub async fn start_machine_process_runtime_with_ports<R, P, L>(
    client: NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    public_ip: Option<IpAddr>,
    observation_interval: Duration,
) -> Result<RunningMachineProcessRuntime, MachineProcessRuntimeError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone
        + crate::machine_runtime::service::MachinePloyzNativeMeshPreparer
        + Send
        + Sync
        + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let machine_service = start_machine_runtime_service_with_public_ip(
        client.clone(),
        machine_id.clone(),
        runner.clone(),
        preparer,
        log_reader,
        public_ip,
    )
    .await
    .map_err(MachineProcessRuntimeError::StartMachineService)?;
    let observer =
        start_machine_observer_runtime(machine_id, runner, client, public_ip, observation_interval);

    Ok(RunningMachineProcessRuntime {
        machine_service,
        observer,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineObserverHealth {
    pub last_attempt: Option<MachineObserverAttempt>,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineObserverAttempt {
    Succeeded,
    Failed { message: String },
}

struct RunningMachineObserverRuntime {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningMachineObserverRuntime {
    async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

fn start_machine_observer_runtime<R>(
    machine_id: MachineId,
    runner: R,
    client: NatsClient,
    public_ip: Option<IpAddr>,
    interval: Duration,
) -> RunningMachineObserverRuntime
where
    R: MachineContainerRunner + Send + Sync + 'static,
{
    let health = Arc::new(Mutex::new(MachineObserverHealth {
        last_attempt: None,
        consecutive_failures: 0,
    }));
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let task_health = Arc::clone(&health);
    let task = tokio::spawn(async move {
        let mut backoff = interval;
        let mut publisher = MachineObservationPublisher::new(client);
        loop {
            let attempt = publisher
                .publish_with_timeout(&machine_id, &runner, public_ip, MACHINE_OBSERVATION_TIMEOUT)
                .await;
            backoff = record_observer_attempt(&task_health, attempt, interval, backoff);
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = &mut shutdown_rx => break,
            }
        }
    });

    RunningMachineObserverRuntime { shutdown, task }
}

fn record_observer_attempt(
    health: &Mutex<MachineObserverHealth>,
    attempt: Result<(), MachineProcessRuntimeError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health
        .lock()
        .expect("machine observer health lock is not poisoned");
    let MachineObserverHealth {
        last_attempt,
        consecutive_failures,
    } = &mut *health;
    let recorded = match attempt {
        Ok(()) => RecordedAttempt::Healthy(MachineObserverAttempt::Succeeded),
        Err(error) => RecordedAttempt::Failed(MachineObserverAttempt::Failed {
            message: error.to_string(),
        }),
    };

    record_attempt(
        last_attempt,
        consecutive_failures,
        recorded,
        BackoffSchedule {
            interval,
            cap: Duration::from_secs(30),
        },
        current_backoff,
    )
}

struct MachineObservationPublisher {
    client: NatsClient,
    observations: LazyHandle<AsyncNatsObservationStore>,
}

impl MachineObservationPublisher {
    fn new(client: NatsClient) -> Self {
        Self {
            client,
            observations: LazyHandle::new(),
        }
    }

    async fn publish_with_timeout<R>(
        &mut self,
        machine_id: &MachineId,
        runner: &R,
        public_ip: Option<IpAddr>,
        timeout: Duration,
    ) -> Result<(), MachineProcessRuntimeError>
    where
        R: MachineContainerRunner,
    {
        tokio::time::timeout(timeout, self.publish(machine_id, runner, public_ip))
            .await
            .map_err(|_| MachineProcessRuntimeError::ObservationTimedOut { timeout })?
    }

    async fn publish<R>(
        &mut self,
        machine_id: &MachineId,
        runner: &R,
        public_ip: Option<IpAddr>,
    ) -> Result<(), MachineProcessRuntimeError>
    where
        R: MachineContainerRunner,
    {
        let client = &self.client;
        let observations = self
            .observations
            .get_or_open(async || {
                let jetstream = async_nats::jetstream::new(client.clone());
                AsyncNatsObservationStore::from_jetstream(&jetstream)
                    .await
                    .map_err(MachineProcessRuntimeError::OpenObservations)
            })
            .await?;

        let facts = read_machine_facts_snapshot(machine_id, runner, public_ip, current_unix_ms())
            .await
            .map_err(MachineProcessRuntimeError::ReadFacts)?;
        observations
            .replace_machine_containers(facts.containers())
            .await
            .map_err(MachineProcessRuntimeError::PublishObservation)?;

        match facts.public_ip() {
            Some(public_ip) => observations.replace_machine_public_ip(public_ip).await,
            None => observations.clear_machine_public_ip(machine_id).await,
        }
        .map_err(MachineProcessRuntimeError::PublishObservation)?;

        Ok(())
    }
}

pub async fn run_machine_until_shutdown(
    config: &MachineProcessConfig,
) -> Result<(), MachineProcessRuntimeError> {
    let runtime = start_machine_process_runtime(config).await?;
    shutdown_signal()
        .await
        .map_err(MachineProcessRuntimeError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(MachineProcessRuntimeError::ShutdownMachineService)
}

#[derive(Debug)]
pub enum MachineProcessRuntimeError {
    AwaitCredentials(AwaitSeedFileError),
    ConnectNats(NatsConnectError),
    OpenObservations(ObservationStoreError),
    ReadFacts(MachineFactsReadError),
    PublishObservation(ObservationStoreError),
    ObservationTimedOut { timeout: Duration },
    StartMachineService(MachineServiceRuntimeError),
    ShutdownSignal(std::io::Error),
    ShutdownMachineService(NatsServiceShutdownError),
}

impl fmt::Display for MachineProcessRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitCredentials(error) => write!(formatter, "{error}"),
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::OpenObservations(error) => {
                write!(formatter, "failed to open observation store: {error}")
            }
            Self::ReadFacts(error) => write!(formatter, "failed to read machine facts: {error}"),
            Self::PublishObservation(error) => {
                write!(formatter, "failed to publish machine observation: {error}")
            }
            Self::ObservationTimedOut { timeout } => {
                write!(
                    formatter,
                    "machine observation publish timed out after {}s",
                    timeout.as_secs()
                )
            }
            Self::StartMachineService(error) => {
                write!(formatter, "failed to start machine service: {error:?}")
            }
            Self::ShutdownSignal(error) => {
                write!(formatter, "failed to wait for shutdown: {error}")
            }
            Self::ShutdownMachineService(error) => {
                write!(formatter, "failed to stop machine service: {error:?}")
            }
        }
    }
}

impl std::error::Error for MachineProcessRuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_runtime::runner::{
        CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
        MachineContainerRunner, MachineContainerRunnerError, MachineLogReader,
        MachineLogReaderError, MachineLogTail,
    };
    use crate::machine_runtime::service::observation_state;
    use ployz_core::dataplane::{
        EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshReady,
        WireGuardEbpfPrepareError, WireGuardReady, WireGuardReadyEvidence,
    };
    use ployz_core::ids::{ContainerId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId};
    use ployz_core::machine_runtime::ManagedContainerKind;
    use ployz_core::machine_runtime::{ContainerRuntimeState, ManagedContainerIdentity};
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
    async fn machine_process_runtime_starts_service_before_observations_are_ready() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = FailingListRunner;

        let runtime = start_machine_process_runtime_with_ports(
            nats.client.clone(),
            machine_id("machine_a"),
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

    impl MachineContainerRunner for StaticRunner {
        async fn existing_managed_containers(
            &self,
        ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
            self.containers
                .lock()
                .map(|containers| containers.clone())
                .map_err(|error| MachineContainerRunnerError::ListExisting {
                    message: error.to_string(),
                })
        }

        async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
            Ok(())
        }

        async fn create_managed_container(
            &self,
            _command: CreateManagedContainer,
        ) -> Result<ContainerId, MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Create {
                message: "not used".to_owned(),
            })
        }

        async fn start_managed_container(
            &self,
            container_id: &ContainerId,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }

        async fn remove_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }

        async fn stop_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }
    }

    #[derive(Debug, Clone)]
    struct FailingListRunner;

    impl MachineContainerRunner for FailingListRunner {
        async fn existing_managed_containers(
            &self,
        ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::ListExisting {
                message: "docker unavailable".to_owned(),
            })
        }

        async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::EnsureEndpointNetwork {
                message: "docker unavailable".to_owned(),
            })
        }

        async fn create_managed_container(
            &self,
            _command: CreateManagedContainer,
        ) -> Result<ContainerId, MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Create {
                message: "not used".to_owned(),
            })
        }

        async fn start_managed_container(
            &self,
            container_id: &ContainerId,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Start {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }

        async fn remove_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }

        async fn stop_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message: "not used".to_owned(),
            })
        }
    }

    impl MachineLogReader for FailingListRunner {
        async fn tail_container_logs(
            &self,
            container_id: &ContainerId,
            _tail_lines: Option<u16>,
        ) -> Result<MachineLogTail, MachineLogReaderError> {
            Err(MachineLogReaderError::ReadFailed {
                container_id: container_id.clone(),
                message: "docker unavailable".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn machine_observation_publish_records_snapshot_when_docker_lists() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = StaticRunner::new([ExistingManagedContainer {
            container_id: container_id("ctr_123"),
            identity: identity_for("run_1"),
            state: ExistingManagedContainerState::Running { ip: None },
        }]);

        let mut publisher = MachineObservationPublisher::new(nats.client.clone());
        publisher
            .publish_with_timeout(
                &machine_id("machine_a"),
                &runner,
                None,
                Duration::from_secs(1),
            )
            .await
            .expect("snapshot publishes");

        let snapshot = nats
            .observations
            .machine_snapshot(&machine_id("machine_a"))
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
    async fn machine_observation_publish_records_configured_public_ip() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = StaticRunner::new([]);

        let mut publisher = MachineObservationPublisher::new(nats.client.clone());
        publisher
            .publish_with_timeout(
                &machine_id("machine_a"),
                &runner,
                Some("203.0.113.7".parse::<IpAddr>().expect("valid public ip")),
                Duration::from_secs(1),
            )
            .await
            .expect("snapshot publishes");

        assert_eq!(
            nats.observations
                .machine_public_ip(&machine_id("machine_a"))
                .await
                .expect("public ip reads")
                .expect("public ip exists")
                .public_ip,
            "203.0.113.7".parse::<IpAddr>().expect("valid public ip")
        );
    }

    #[tokio::test]
    async fn machine_observation_publish_clears_public_ip_when_unset() {
        let nats = TestNats::start_bootstrapped().await;
        let runner = StaticRunner::new([]);
        let mut publisher = MachineObservationPublisher::new(nats.client.clone());
        let machine_id = machine_id("machine_a");

        publisher
            .publish_with_timeout(
                &machine_id,
                &runner,
                Some("203.0.113.7".parse::<IpAddr>().expect("valid public ip")),
                Duration::from_secs(1),
            )
            .await
            .expect("public ip publishes");
        publisher
            .publish_with_timeout(&machine_id, &runner, None, Duration::from_secs(1))
            .await
            .expect("public ip clears");

        assert_eq!(
            nats.observations
                .machine_public_ip(&machine_id)
                .await
                .expect("public ip reads"),
            None
        );
    }

    #[derive(Clone)]
    struct ReadyWireGuardEbpf;

    impl crate::machine_runtime::service::MachinePloyzNativeMeshPreparer for ReadyWireGuardEbpf {
        async fn read_wireguard_public_key(
            &self,
        ) -> Result<ployz_core::dataplane::WireGuardPublicKey, WireGuardEbpfPrepareError> {
            ployz_core::dataplane::WireGuardPublicKey::try_new("test-public-key").map_err(
                |source| WireGuardEbpfPrepareError::InvalidReport {
                    message: ployz_core::ops::FailureMessage::try_new(source.to_string())
                        .expect("wireguard public key error is non-empty"),
                },
            )
        }

        async fn prepare_ployz_native_mesh(
            &self,
            _endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
            _peers: &[ployz_core::dataplane::WireGuardPeer],
        ) -> Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError> {
            Ok(PloyzNativeMeshReady {
                wireguard: WireGuardReady {
                    public_key: ployz_core::dataplane::WireGuardPublicKey::try_new(
                        "test-public-key",
                    )
                    .expect("test public key is valid"),
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
        _nats: ployz_test_support::nats::TestNats,
        /// Machine principal: the machine process side under test.
        client: async_nats::Client,
        observations: AsyncNatsObservationStore,
    }

    impl TestNats {
        async fn start_bootstrapped() -> Self {
            let nats =
                ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")])
                    .await;
            nats.bootstrap_resources().await;
            let client = nats.machine_client(&machine_id("machine_a")).await;
            let machine_jetstream = async_nats::jetstream::new(client.clone());
            let observations = AsyncNatsObservationStore::from_jetstream(&machine_jetstream)
                .await
                .expect("open observations");

            Self {
                _nats: nats,
                client,
                observations,
            }
        }
    }

    fn identity_for(step: &str) -> ManagedContainerIdentity {
        ManagedContainerIdentity {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_2"),
            operation_id: operation_id("op_123"),
            step_id: step_id(step),
            kind: ManagedContainerKind::Service,
        }
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn namespace_id(value: &str) -> ployz_core::ids::NamespaceId {
        ployz_core::ids::NamespaceId::try_new(value).expect("valid namespace id")
    }

    fn service_id(value: &str) -> ServiceId {
        ServiceId::try_new(value).expect("valid service id")
    }

    fn namespace_revision_entry_id(value: &str) -> NamespaceRevisionEntryId {
        NamespaceRevisionEntryId::try_new(value).expect("valid namespace revision entry id")
    }

    fn step_id(value: &str) -> StepId {
        StepId::try_new(value).expect("valid step id")
    }
}
