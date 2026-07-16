//! Process wiring for the machine role.

use crate::adapters::credentials::{
    AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials,
};
use crate::config::MachineProcessConfig;
use crate::process_support::{
    BackoffSchedule, RecordedAttempt, bounded_role_shutdown, record_attempt, shutdown_signal,
};
use crate::recovery::{IntentFailover, mirrored_server_pool, spawn_intent_failover_mirror};
use crate::recovery::{IntentMirror, PendingMachineJoinMirror};
use crate::roles::machine::build::MachineBuildRuntime;
use crate::roles::machine::execution::build::{BuildExecutionError, DockerBuildExecutor};
use crate::roles::machine::execution::containerd_content::ContainerdContentStore;
use crate::roles::machine::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::machine::execution::host_dataplane::{
    PloyzNativeMeshHostConfig, PloyzNativeMeshPreparer, WireGuardMtuPolicy,
};
use crate::roles::machine::execution::registry_v2::RunningRegistryV2;
use crate::roles::machine::facts::{
    MachineEndpointCache, MachineFactsPublishError, publish_machine_facts,
};
use crate::roles::machine::images::AvailableImageService;
use crate::roles::machine::projection::{
    MachineProjectionState, RunningProjectionTask, start_projection_task,
};
use crate::roles::machine::runner::{
    MachineContainerRunner, MachineImageRemovalRunner, MachineLogReader, MachineVolumeUsageReader,
};
use crate::roles::machine::service::{
    MachineFactsReadError, MachineRoleProjectionServices, MachineServiceError,
    start_machine_role_service_with_endpoint_cache_and_image,
};
use futures_util::StreamExt;
use ployz_core::build::{BUILD_FORCE_CLEANUP_TIMEOUT, BUILD_TASK_DRAIN_TIMEOUT};
use ployz_core::ids::MachineId;
use ployz_core::image::IMAGE_MESH_REGISTRY_PORT;
use ployz_core::intent::recovery::PendingMachineJoinRecoverySnapshot;
use ployz_core::network::MachineEndpointSubnet;
use ployz_nats::connect::{NatsClientUrl, NatsConnectError, connect_authenticated_pool};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError, RunningNatsService};
use ployz_nats::subjects::PENDING_MACHINE_JOINS_CHANGED;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

const MACHINE_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MACHINE_OBSERVATION_INTERVAL: Duration =
    ployz_core::machine::runtime::OBSERVATION_PUBLISH_INTERVAL;
const MACHINE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);
const MACHINE_SHUTDOWN_TIMEOUT: Duration = BUILD_TASK_DRAIN_TIMEOUT
    .saturating_add(BUILD_TASK_DRAIN_TIMEOUT)
    .saturating_add(BUILD_FORCE_CLEANUP_TIMEOUT)
    .saturating_add(Duration::from_secs(5));
const INTENT_MIRROR_RESUBSCRIBE_DELAY: Duration = Duration::from_secs(5);
const BUILD_WORKSPACE_ROOT: &str = "/var/lib/ployz/builds";

pub struct RunningMachineProcess {
    machine_service: RunningNatsService,
    observer: RunningTask,
    intent_mirror_shutdown: broadcast::Sender<()>,
    intent_mirror: JoinHandle<()>,
    pending_join_mirror: RunningTask,
    projection: RunningProjectionTask,
    image_registry: Option<RunningRegistryV2>,
    build_runtime: Option<MachineBuildRuntime>,
}

impl RunningMachineProcess {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        let Self {
            machine_service,
            mut observer,
            intent_mirror_shutdown,
            mut intent_mirror,
            mut pending_join_mirror,
            mut projection,
            image_registry,
            build_runtime,
        } = self;
        pending_join_mirror.request_shutdown();
        projection.request_shutdown();
        let _ = intent_mirror_shutdown.send(());
        observer.request_shutdown();
        let abort_handles = [
            pending_join_mirror.task.abort_handle(),
            projection.abort_handle(),
            intent_mirror.abort_handle(),
            observer.task.abort_handle(),
        ];
        let cleanup = async {
            if let Some(build_runtime) = build_runtime {
                build_runtime.shutdown().await;
            }
            if let Some(image_registry) = image_registry {
                image_registry.shutdown().await;
            }
            pending_join_mirror.wait().await;
            projection.wait().await;
            let _ = (&mut intent_mirror).await;
            observer.wait().await;
            machine_service.shutdown().await
        };
        bounded_role_shutdown("machine", MACHINE_SHUTDOWN_TIMEOUT, &abort_handles, cleanup).await
    }
}

pub async fn start_machine_process(
    config: &MachineProcessConfig,
) -> Result<RunningMachineProcess, MachineProcessError> {
    // The machine credential may not exist yet (first machine before
    // activate-first-machine): awaiting it is a typed bounded-retry state,
    // not a crash loop.
    let connect = await_role_credentials(
        "machine",
        &config.nats,
        &SeedFileRetryPolicy::default_policy(),
    )
    .await
    .map_err(MachineProcessError::AwaitCredentials)?;
    // Build the failover pool from the persisted mirror *before* connecting, so a
    // machine rebooting during a core outage dials a promoted core from its cached
    // roster rather than timing out on the possibly-dead configured seed.
    let intent_mirror = IntentMirror::beside_seed_file(&config.nats.seed_file);
    let pending_join_mirror = PendingMachineJoinMirror::new(
        config
            .nats
            .seed_file
            .with_file_name("pending-machine-joins.json"),
    );
    let seed = connect.url.clone();
    let initial_pool = mirrored_server_pool(&intent_mirror, &seed);
    let client = connect_authenticated_pool(&connect, &initial_pool, MACHINE_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(MachineProcessError::ConnectNats)?;
    let mtu_policy = WireGuardMtuPolicy::from_config(config.ployz_native_mesh.wg_mtu)
        .map_err(|message| MachineProcessError::InvalidDataplaneMtu { message })?;
    let runner = DockerManagedContainerRunner::lazy_local_defaults(
        config.ployz_native_mesh.endpoint_subnet.clone(),
        config.ployz_native_mesh.bridge_ifname.clone(),
        config.ployz_native_mesh.wg_ifname.clone(),
        mtu_policy,
    );
    let endpoint_subnet = MachineEndpointSubnet::try_new(&config.ployz_native_mesh.endpoint_subnet)
        .map_err(|error| MachineProcessError::InvalidImageRegistryAddress {
            message: error.to_string(),
        })?;
    let (image_state, image_registry) = match ContainerdContentStore::connect_docker_default().await
    {
        Ok(image_content) => {
            let registry = RunningRegistryV2::spawn_retrying(
                SocketAddr::from((endpoint_subnet.host_address(), IMAGE_MESH_REGISTRY_PORT)),
                image_content.clone(),
            );
            (
                Some(AvailableImageService::new(
                    image_content,
                    runner.clone(),
                    endpoint_subnet.host_address(),
                )),
                Some(registry),
            )
        }
        Err(error) => {
            eprintln!(
                "ployzd machine image service unavailable; facts and container RPCs remain active: {error}"
            );
            (None, None)
        }
    };
    let preparer = PloyzNativeMeshPreparer::new(
        PloyzNativeMeshHostConfig::with_default_key_material(
            config.machine_id.clone(),
            config.artifacts.ebpf_bytecode_path.clone(),
            config.artifacts.ebpf_ctl_path.clone(),
            config.ployz_native_mesh.bridge_ifname.clone(),
            config.ployz_native_mesh.wg_ifname.clone(),
        )
        .with_mtu_policy(mtu_policy),
    );

    let mut process = start_machine_process_with_ports(
        client,
        config.machine_id.clone(),
        runner.clone(),
        preparer,
        runner,
        intent_mirror,
        pending_join_mirror,
        seed,
        MACHINE_OBSERVATION_INTERVAL,
        config.ployz_native_mesh.wg_ifname.clone(),
        image_state,
    )
    .await?;
    process.image_registry = image_registry;
    Ok(process)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn start_machine_process_with_ports<R, P, L>(
    client: NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    intent_mirror: IntentMirror,
    pending_join_mirror: PendingMachineJoinMirror,
    seed: NatsClientUrl,
    observation_interval: Duration,
    wg_ifname: String,
    image_state: Option<AvailableImageService>,
) -> Result<RunningMachineProcess, MachineProcessError>
where
    R: Clone
        + MachineContainerRunner
        + MachineImageRemovalRunner
        + MachineVolumeUsageReader
        + Send
        + Sync
        + 'static,
    P: Clone
        + crate::roles::machine::service::MachinePloyzNativeMeshPreparer
        + Send
        + Sync
        + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let endpoint_cache = MachineEndpointCache::new(wg_ifname);
    let projection_state = MachineProjectionState::new();
    let build_state = match image_state.clone() {
        Some(images) => {
            let executor = DockerBuildExecutor::new(
                machine_id.clone(),
                client.clone(),
                PathBuf::from(BUILD_WORKSPACE_ROOT),
            );
            let runtime = MachineBuildRuntime::new(machine_id.clone(), executor, Some(images))
                .map_err(|message| MachineProcessError::InitializeBuildRuntime { message })?;
            runtime
                .recover_orphans()
                .await
                .map_err(MachineProcessError::RecoverBuildRuntime)?;
            Some(runtime)
        }
        None => None,
    };
    let machine_service = start_machine_role_service_with_endpoint_cache_and_image(
        client.clone(),
        machine_id.clone(),
        runner.clone(),
        preparer.clone(),
        log_reader,
        endpoint_cache.clone(),
        MachineRoleProjectionServices {
            build_state: build_state.clone(),
            image_state,
            projection_state: projection_state.clone(),
        },
    )
    .await
    .map_err(MachineProcessError::StartMachineService)?;
    let build_runtime = build_state;
    let (intent_mirror_shutdown, intent_mirror_shutdown_rx) = broadcast::channel(1);
    let intent_mirror = spawn_intent_failover_mirror(
        client.clone(),
        IntentFailover {
            mirror: intent_mirror,
            seed,
        },
        intent_mirror_shutdown_rx,
    );
    let pending_join_mirror = start_pending_join_mirror(client.clone(), pending_join_mirror);
    let projection = start_projection_task(
        client.clone(),
        machine_id.clone(),
        runner.clone(),
        preparer,
        projection_state,
    );
    let observer = start_machine_observer(
        machine_id,
        runner,
        client,
        observation_interval,
        endpoint_cache,
    );

    Ok(RunningMachineProcess {
        machine_service,
        observer,
        intent_mirror_shutdown,
        intent_mirror,
        pending_join_mirror,
        projection,
        image_registry: None,
        build_runtime,
    })
}

/// A background task owned by the machine process: a shutdown signal and its
/// join handle. Shared by the observer and the pending-join mirror.
struct RunningTask {
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl RunningTask {
    fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    async fn wait(&mut self) {
        let _ = (&mut self.task).await;
    }
}

fn start_pending_join_mirror(client: NatsClient, mirror: PendingMachineJoinMirror) -> RunningTask {
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let mut changed = match client.subscribe(PENDING_MACHINE_JOINS_CHANGED).await {
                Ok(subscription) => subscription,
                Err(_) => {
                    tokio::select! {
                        () = tokio::time::sleep(INTENT_MIRROR_RESUBSCRIBE_DELAY) => continue,
                        _ = &mut shutdown_rx => return,
                    }
                }
            };
            loop {
                tokio::select! {
                    message = changed.next() => {
                        let Some(message) = message else { break };
                        if let Ok(snapshot) =
                            serde_json::from_slice::<PendingMachineJoinRecoverySnapshot>(&message.payload)
                        {
                            let _ = mirror.store(&snapshot);
                        }
                    }
                    _ = &mut shutdown_rx => return,
                }
            }
        }
    });
    RunningTask {
        shutdown: Some(shutdown),
        task,
    }
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

fn start_machine_observer<R>(
    machine_id: MachineId,
    runner: R,
    client: NatsClient,
    interval: Duration,
    endpoint_cache: MachineEndpointCache,
) -> RunningTask
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
        let mut publisher = MachineObservationPublisher::new(client, endpoint_cache);
        loop {
            let attempt = tokio::select! {
                attempt = publisher.publish_with_timeout(
                    &machine_id,
                    &runner,
                    MACHINE_OBSERVATION_TIMEOUT,
                ) => attempt,
                _ = &mut shutdown_rx => break,
            };
            backoff = record_observer_attempt(&task_health, attempt, interval, backoff);
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = &mut shutdown_rx => break,
            }
        }
    });

    RunningTask {
        shutdown: Some(shutdown),
        task,
    }
}

fn record_observer_attempt(
    health: &Mutex<MachineObserverHealth>,
    attempt: Result<(), MachineProcessError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    endpoint_cache: MachineEndpointCache,
}

impl MachineObservationPublisher {
    fn new(client: NatsClient, endpoint_cache: MachineEndpointCache) -> Self {
        Self {
            client,
            endpoint_cache,
        }
    }

    async fn publish_with_timeout<R>(
        &mut self,
        machine_id: &MachineId,
        runner: &R,
        timeout: Duration,
    ) -> Result<(), MachineProcessError>
    where
        R: MachineContainerRunner,
    {
        tokio::time::timeout(timeout, self.publish(machine_id, runner))
            .await
            .map_err(|_| MachineProcessError::ObservationTimedOut { timeout })?
    }

    async fn publish<R>(
        &mut self,
        machine_id: &MachineId,
        runner: &R,
    ) -> Result<(), MachineProcessError>
    where
        R: MachineContainerRunner,
    {
        publish_machine_facts(&self.client, machine_id, runner, &self.endpoint_cache)
            .await
            .map(|_| ())
            .map_err(|error| match error {
                MachineFactsPublishError::Read(error) => MachineProcessError::ReadFacts(error),
                MachineFactsPublishError::Encode(error) => MachineProcessError::EncodeFacts(error),
                MachineFactsPublishError::Publish { message } => {
                    MachineProcessError::PublishFacts { message }
                }
            })
    }
}

pub async fn run_machine_until_shutdown(
    config: &MachineProcessConfig,
) -> Result<(), MachineProcessError> {
    let shutdown = shutdown_signal().map_err(MachineProcessError::ShutdownSignal)?;
    let runtime = start_machine_process(config).await?;
    shutdown
        .await
        .map_err(MachineProcessError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(MachineProcessError::ShutdownMachineService)
}

#[derive(Debug, thiserror::Error)]
pub enum MachineProcessError {
    #[error("{0}")]
    AwaitCredentials(AwaitSeedFileError),
    #[error("{0}")]
    ConnectNats(NatsConnectError),
    #[error("failed to read machine facts: {0}")]
    ReadFacts(MachineFactsReadError),
    #[error("failed to encode machine facts: {0}")]
    EncodeFacts(serde_json::Error),
    #[error("failed to publish machine facts: {message}")]
    PublishFacts { message: String },
    #[error("machine observation publish timed out after {}s", timeout.as_secs())]
    ObservationTimedOut { timeout: Duration },
    #[error("invalid dataplane WireGuard MTU: {message}")]
    InvalidDataplaneMtu { message: String },
    #[error("invalid image registry address: {message}")]
    InvalidImageRegistryAddress { message: String },
    #[error("failed to initialize machine build runtime: {message}")]
    InitializeBuildRuntime { message: String },
    #[error("failed to recover machine build runtime: {0}")]
    RecoverBuildRuntime(BuildExecutionError),
    #[error("failed to start machine service: {0:?}")]
    StartMachineService(MachineServiceError),
    #[error("failed to wait for shutdown: {0}")]
    ShutdownSignal(std::io::Error),
    #[error("failed to stop machine service: {0:?}")]
    ShutdownMachineService(NatsServiceShutdownError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::machine::facts::observation_state;
    use crate::roles::machine::runner::{
        CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
        MachineContainerRunner, MachineContainerRunnerError, MachineImageRemovalRunner,
        MachineLogReader, MachineLogReaderError, MachineLogTail, MachineVolumeUsageReader,
    };
    use futures_util::StreamExt;
    use ployz_core::ids::{ContainerId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId};
    use ployz_core::machine::runtime::ManagedContainerKind;
    use ployz_core::machine::runtime::{
        ContainerRuntimeState, MachineFactsSnapshot, ManagedContainerIdentity,
    };
    use ployz_core::network::{
        EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshReady,
        WireGuardEbpfPrepareError, WireGuardReady, WireGuardReadyEvidence,
    };
    use ployz_nats::subjects::machine_facts;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

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

        let mirror_dir = tempfile::tempdir().expect("temp dir");
        let runtime = start_machine_process_with_ports(
            nats.client.clone(),
            machine_id("machine_a"),
            runner.clone(),
            ReadyWireGuardEbpf,
            runner,
            IntentMirror::new(mirror_dir.path().join("intent-mirror.json")),
            PendingMachineJoinMirror::new(mirror_dir.path().join("pending-machine-joins.json")),
            NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("seed url"),
            Duration::from_secs(60),
            "ployz-wg0".to_owned(),
            None,
        )
        .await
        .expect("runtime starts");

        runtime.shutdown().await.expect("runtime shuts down");
    }

    #[derive(Debug, Clone)]
    struct StaticRunner {
        containers: StaticContainerList,
    }

    impl StaticRunner {
        fn new(containers: impl IntoIterator<Item = ExistingManagedContainer>) -> Self {
            Self {
                containers: StaticContainerList::Ready(Arc::new(Mutex::new(
                    containers.into_iter().collect(),
                ))),
            }
        }

        fn blocking() -> (Self, BlockedList) {
            let blocked_list = BlockedList::default();
            (
                Self {
                    containers: StaticContainerList::Blocked(blocked_list.clone()),
                },
                blocked_list,
            )
        }
    }

    #[derive(Debug, Clone)]
    enum StaticContainerList {
        Ready(Arc<Mutex<Vec<ExistingManagedContainer>>>),
        Blocked(BlockedList),
    }

    #[derive(Debug, Clone, Default)]
    struct BlockedList {
        started: Arc<Notify>,
        cancelled: Arc<Notify>,
    }

    struct NotifyOnDrop(Arc<Notify>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    impl MachineImageRemovalRunner for StaticRunner {
        async fn remove_image(
            &self,
            _image_identity: &ployz_core::image::OciDigest,
        ) -> Result<ployz_core::image::ImageRemoveOutcome, String> {
            Ok(ployz_core::image::ImageRemoveOutcome::AlreadyAbsent)
        }
    }

    impl MachineContainerRunner for StaticRunner {
        async fn ensure_volume(
            &self,
            _volume: &ployz_core::intent::VolumePinState,
        ) -> Result<(), ployz_core::machine::VolumeEnsureFailure> {
            Ok(())
        }

        async fn existing_managed_containers(
            &self,
        ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
            match &self.containers {
                StaticContainerList::Ready(containers) => containers
                    .lock()
                    .map(|containers| containers.clone())
                    .map_err(|error| MachineContainerRunnerError::ListExisting {
                        message: error.to_string(),
                    }),
                StaticContainerList::Blocked(blocked) => {
                    blocked.started.notify_one();
                    let _cancelled = NotifyOnDrop(Arc::clone(&blocked.cancelled));
                    std::future::pending().await
                }
            }
        }

        async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
            Ok(())
        }

        async fn ensure_projection_endpoint_network(
            &self,
            _expected_subnet: &ployz_core::network::MachineEndpointSubnet,
        ) -> Result<(), MachineContainerRunnerError> {
            Ok(())
        }

        async fn read_endpoint_network_status(&self) -> ployz_core::network::EndpointBridgeStatus {
            ployz_core::network::EndpointBridgeStatus::Missing
        }

        async fn resolve_registry_image(
            &self,
            reference: &ployz_core::deploy::ImageReference,
            _credential: Option<&ployz_core::deploy::RegistryCredential>,
        ) -> Result<ployz_core::image::OciDigest, MachineContainerRunnerError> {
            Ok(ployz_core::image::OciDigest::sha256(
                reference.as_str().as_bytes(),
            ))
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

        async fn wait_managed_container(
            &self,
            _container_id: &ContainerId,
        ) -> Result<i64, MachineContainerRunnerError> {
            Ok(0)
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

        async fn remove_volume(
            &self,
            docker_volume_name: &str,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::RemoveVolume {
                docker_volume_name: docker_volume_name.to_owned(),
                message: "not used".to_owned(),
            })
        }

        async fn destroy_provisioned_dataset(
            &self,
            dataset: &ployz_core::deploy::DatasetName,
        ) -> Result<(), ployz_core::storage::StorageEffectFailure> {
            Err(ployz_core::storage::StorageEffectFailure::ProcessFailed {
                message: format!("dataset destroy not used: {}", dataset.as_str()),
            })
        }

        async fn restart_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Restart {
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

    impl MachineVolumeUsageReader for StaticRunner {
        async fn read_volume_usage(
            &self,
            _volume: &ployz_core::intent::VolumePinState,
        ) -> Option<ployz_core::machine::VolumeUsageFacts> {
            None
        }
    }

    #[tokio::test]
    async fn machine_observer_shutdown_cancels_blocked_fact_collection() {
        let nats = TestNats::start_bootstrapped().await;
        let (runner, blocked) = StaticRunner::blocking();
        let mut observer = start_machine_observer(
            machine_id("machine_a"),
            runner,
            nats.client.clone(),
            Duration::from_secs(60),
            MachineEndpointCache::default(),
        );
        tokio::time::timeout(Duration::from_secs(1), blocked.started.notified())
            .await
            .expect("fact collection starts");

        observer.request_shutdown();
        tokio::time::timeout(Duration::from_secs(1), observer.wait())
            .await
            .expect("observer shutdown cancels fact collection");
        tokio::time::timeout(Duration::from_secs(1), blocked.cancelled.notified())
            .await
            .expect("blocked fact collection future is dropped");
    }

    #[derive(Debug, Clone)]
    struct FailingListRunner;

    impl MachineImageRemovalRunner for FailingListRunner {
        async fn remove_image(
            &self,
            _image_identity: &ployz_core::image::OciDigest,
        ) -> Result<ployz_core::image::ImageRemoveOutcome, String> {
            Ok(ployz_core::image::ImageRemoveOutcome::AlreadyAbsent)
        }
    }

    impl MachineContainerRunner for FailingListRunner {
        async fn ensure_volume(
            &self,
            _volume: &ployz_core::intent::VolumePinState,
        ) -> Result<(), ployz_core::machine::VolumeEnsureFailure> {
            Ok(())
        }

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

        async fn ensure_projection_endpoint_network(
            &self,
            _expected_subnet: &ployz_core::network::MachineEndpointSubnet,
        ) -> Result<(), MachineContainerRunnerError> {
            self.ensure_endpoint_network().await
        }

        async fn read_endpoint_network_status(&self) -> ployz_core::network::EndpointBridgeStatus {
            ployz_core::network::EndpointBridgeStatus::Unavailable {
                message: ployz_core::operation::FailureMessage::try_new("docker unavailable")
                    .expect("failure"),
            }
        }

        async fn resolve_registry_image(
            &self,
            _reference: &ployz_core::deploy::ImageReference,
            _credential: Option<&ployz_core::deploy::RegistryCredential>,
        ) -> Result<ployz_core::image::OciDigest, MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::ImagePull {
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

        async fn wait_managed_container(
            &self,
            _container_id: &ContainerId,
        ) -> Result<i64, MachineContainerRunnerError> {
            Ok(0)
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

        async fn remove_volume(
            &self,
            docker_volume_name: &str,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::RemoveVolume {
                docker_volume_name: docker_volume_name.to_owned(),
                message: "not used".to_owned(),
            })
        }

        async fn destroy_provisioned_dataset(
            &self,
            dataset: &ployz_core::deploy::DatasetName,
        ) -> Result<(), ployz_core::storage::StorageEffectFailure> {
            Err(ployz_core::storage::StorageEffectFailure::ProcessFailed {
                message: format!("dataset destroy not used: {}", dataset.as_str()),
            })
        }

        async fn restart_managed_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &ManagedContainerIdentity,
        ) -> Result<(), MachineContainerRunnerError> {
            Err(MachineContainerRunnerError::Restart {
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

    impl MachineVolumeUsageReader for FailingListRunner {
        async fn read_volume_usage(
            &self,
            _volume: &ployz_core::intent::VolumePinState,
        ) -> Option<ployz_core::machine::VolumeUsageFacts> {
            None
        }
    }

    impl MachineLogReader for FailingListRunner {
        async fn tail_container_logs(
            &self,
            container_id: &ContainerId,
            _query: crate::roles::machine::runner::MachineLogQuery,
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
            state: ExistingManagedContainerState::Running {
                ip: None,
                health: ployz_core::machine::runtime::ContainerHealth::None,
                started_at_unix_ms: None,
            },
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
        }]);
        let mut facts_sub = nats
            .client
            .subscribe(machine_facts(&machine_id("machine_a")))
            .await
            .expect("subscribe machine facts");

        let mut publisher =
            MachineObservationPublisher::new(nats.client.clone(), MachineEndpointCache::default());
        publisher
            .publish_with_timeout(&machine_id("machine_a"), &runner, Duration::from_secs(1))
            .await
            .expect("snapshot publishes");

        let facts = next_published_facts(&mut facts_sub).await;
        let snapshot = facts.containers();
        assert_eq!(snapshot.containers().len(), 1);
        assert_eq!(
            snapshot
                .container(&container_id("ctr_123"))
                .expect("container exists")
                .state,
            ContainerRuntimeState::running_unroutable()
        );
    }

    async fn next_published_facts(facts_sub: &mut async_nats::Subscriber) -> MachineFactsSnapshot {
        let message = tokio::time::timeout(Duration::from_secs(1), facts_sub.next())
            .await
            .expect("machine facts publish arrives")
            .expect("machine facts subscription stays open");
        serde_json::from_slice(&message.payload).expect("machine facts decode")
    }

    #[derive(Clone)]
    struct ReadyWireGuardEbpf;

    impl crate::roles::machine::service::MachinePloyzNativeMeshPreparer for ReadyWireGuardEbpf {
        async fn read_ployz_native_mesh_status(
            &self,
            _mode: ployz_core::network::NetworkStatusMode,
        ) -> Result<ployz_core::network::MachineDataplaneStatus, String> {
            Err("dataplane status is unavailable".to_owned())
        }

        async fn read_wireguard_public_key(
            &self,
        ) -> Result<ployz_core::network::WireGuardPublicKey, WireGuardEbpfPrepareError> {
            ployz_core::network::WireGuardPublicKey::try_new("test-public-key").map_err(|source| {
                WireGuardEbpfPrepareError::InvalidReport {
                    message: ployz_core::operation::FailureMessage::try_new(source.to_string())
                        .expect("wireguard public key error is non-empty"),
                }
            })
        }

        async fn prepare_ployz_native_mesh(
            &self,
            _endpoint_routes: &[ployz_core::network::WireGuardEbpfEndpointRoute],
            _peers: &[ployz_core::network::WireGuardPeer],
        ) -> Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError> {
            Ok(PloyzNativeMeshReady {
                wireguard: WireGuardReady {
                    public_key: ployz_core::network::WireGuardPublicKey::try_new("test-public-key")
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

        async fn prepare_wireguard(
            &self,
            endpoint_routes: &[ployz_core::network::WireGuardEbpfEndpointRoute],
            peers: &[ployz_core::network::WireGuardPeer],
        ) -> Result<WireGuardReady, WireGuardEbpfPrepareError> {
            self.prepare_ployz_native_mesh(endpoint_routes, peers)
                .await
                .map(|ready| ready.wireguard)
        }
    }

    struct TestNats {
        _nats: ployz_test_support::nats::TestNats,
        /// Machine principal: the machine process side under test.
        client: async_nats::Client,
    }

    impl TestNats {
        async fn start_bootstrapped() -> Self {
            let nats =
                ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")])
                    .await;
            let client = nats.machine_client(&machine_id("machine_a")).await;

            Self {
                _nats: nats,
                client,
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
