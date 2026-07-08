//! Process wiring for the machine role.

use crate::adapters::credentials::{
    AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials,
};
use crate::adapters::docker::runner::DockerManagedContainerRunner;
use crate::adapters::host_dataplane::{
    PloyzNativeMeshHostConfig, PloyzNativeMeshPreparer, WireGuardMtuPolicy,
};
use crate::config::MachineProcessConfig;
use crate::process_support::{BackoffSchedule, RecordedAttempt, record_attempt, shutdown_signal};
use crate::roles::machine::intent_mirror::{MachineIntentMirror, MachinePendingJoinMirror};
use crate::roles::machine::runner::{MachineContainerRunner, MachineLogReader};
use crate::roles::machine::service::{
    MachineEndpointCache, MachineFactsReadError, MachineServiceError, current_unix_ms,
    read_machine_facts_snapshot, refresh_machine_endpoints,
    start_machine_role_service_with_endpoint_cache,
};
use futures_util::StreamExt;
use ployz_core::ids::MachineId;
use ployz_core::state::{IntentSnapshot, PendingMachineJoinRecoverySnapshot};
use ployz_core::subjects::{INTENT_CHANGED, PENDING_MACHINE_JOINS_CHANGED, machine_facts};
use ployz_nats::connect::{NatsClientUrl, NatsConnectError, connect_authenticated_pool};
use ployz_nats::service_runtime::{NatsClient, NatsServiceShutdownError, RunningNatsService};
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

const MACHINE_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MACHINE_OBSERVATION_INTERVAL: Duration =
    ployz_core::machine_runtime::OBSERVATION_PUBLISH_INTERVAL;
const MACHINE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(5);
const INTENT_MIRROR_RESUBSCRIBE_DELAY: Duration = Duration::from_secs(5);
/// Minimum spacing between forced reconnects when a machine detects it is pinned
/// to a lower-epoch (healed old) core — enough to rotate onto a higher-epoch
/// candidate without a reconnect storm.
const EPOCH_ENFORCE_INTERVAL: Duration = Duration::from_secs(5);
/// The port a promoted core's `nats-server` listens on (keeper's
/// `DEFAULT_NATS_PORT`), used to build failover candidate URLs.
const CORE_NATS_PORT: u16 = 4222;

pub struct RunningMachineProcess {
    machine_service: RunningNatsService,
    observer: RunningTask,
    intent_mirror: RunningTask,
    pending_join_mirror: RunningTask,
}

impl RunningMachineProcess {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        self.pending_join_mirror.shutdown().await;
        self.intent_mirror.shutdown().await;
        self.observer.shutdown().await;
        self.machine_service.shutdown().await
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
    let intent_mirror =
        MachineIntentMirror::new(config.nats.seed_file.with_file_name("intent-mirror.json"));
    let pending_join_mirror = MachinePendingJoinMirror::new(
        config
            .nats
            .seed_file
            .with_file_name("pending-machine-joins.json"),
    );
    let seed = connect.url.clone();
    let initial_pool = mirrored_server_pool(&config.nats.seed_file, &seed);
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

    start_machine_process_with_ports(
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
    )
    .await
}

// ponytail: a test-injection wiring seam — three generic ports plus runtime
// config. Bundling would add a generic struct used by exactly two call sites.
#[allow(clippy::too_many_arguments)]
pub async fn start_machine_process_with_ports<R, P, L>(
    client: NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    log_reader: L,
    intent_mirror: MachineIntentMirror,
    pending_join_mirror: MachinePendingJoinMirror,
    seed: NatsClientUrl,
    observation_interval: Duration,
    wg_ifname: String,
) -> Result<RunningMachineProcess, MachineProcessError>
where
    R: Clone + MachineContainerRunner + Send + Sync + 'static,
    P: Clone
        + crate::roles::machine::service::MachinePloyzNativeMeshPreparer
        + Send
        + Sync
        + 'static,
    L: Clone + MachineLogReader + Send + Sync + 'static,
{
    let endpoint_cache = MachineEndpointCache::new(wg_ifname);
    let machine_service = start_machine_role_service_with_endpoint_cache(
        client.clone(),
        machine_id.clone(),
        runner.clone(),
        preparer,
        log_reader,
        endpoint_cache.clone(),
    )
    .await
    .map_err(MachineProcessError::StartMachineService)?;
    let intent_mirror = start_intent_mirror(client.clone(), intent_mirror, seed);
    let pending_join_mirror = start_pending_join_mirror(client.clone(), pending_join_mirror);
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
        intent_mirror,
        pending_join_mirror,
    })
}

/// A background task owned by the machine process: a shutdown signal and its
/// join handle. Shared by the observer and the intent mirror.
struct RunningTask {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningTask {
    async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// The NATS failover pool a machine cycles on core loss: its configured core
/// (the `seed`, always first and always retained) plus every Reachable Machine's
/// endpoint from the mirror. The seed is never dropped by a reachability change —
/// only a reconfigure replaces it — so a transient core outage can't cut the
/// machine off from a still-alive core.
fn reachable_machine_urls(snapshot: &IntentSnapshot) -> Vec<String> {
    let mut urls = Vec::new();
    for machine in &snapshot.active_machines {
        for endpoint in &machine.control_endpoints {
            // SocketAddr's Display brackets IPv6 (`[::1]:4222`); a bare interpolation
            // would emit an invalid `tls://::1:4222`.
            let url = format!("tls://{}", SocketAddr::new(*endpoint, CORE_NATS_PORT));
            if !urls.contains(&url) {
                urls.push(url);
            }
        }
    }
    urls
}

fn candidate_server_pool(snapshot: &IntentSnapshot, seed: &NatsClientUrl) -> Vec<String> {
    let mut pool = vec![seed.as_str().to_owned()];
    for url in reachable_machine_urls(snapshot) {
        if !pool.contains(&url) {
            pool.push(url);
        }
    }
    pool
}

pub(crate) fn mirrored_server_pool(seed_file: &Path, seed: &NatsClientUrl) -> Vec<String> {
    let mirror = MachineIntentMirror::new(seed_file.with_file_name("intent-mirror.json"));
    match mirror.load() {
        Some(snapshot) => candidate_server_pool(&snapshot, seed),
        None => vec![seed.as_str().to_owned()],
    }
}

pub(crate) fn start_mirrored_server_pool_refresh(
    client: NatsClient,
    seed_file: PathBuf,
    seed: NatsClientUrl,
    interval: Duration,
    mut shutdown: broadcast::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = tokio::time::sleep(interval) => {
                    let _ = client.set_server_pool(mirrored_server_pool(&seed_file, &seed)).await;
                }
                _ = shutdown.recv() => break,
            }
        }
    })
}

/// Like [`candidate_server_pool`] but with the configured seed deprioritized to
/// last, used when that seed is a stale (lower-epoch) core we want to reconnect
/// away from: `retain_servers_order` then tries every Reachable Machine (the
/// promoted core among them) before falling back to the stale seed.
fn candidate_server_pool_seed_last(snapshot: &IntentSnapshot, seed: &NatsClientUrl) -> Vec<String> {
    let seed = seed.as_str().to_owned();
    // The stale seed is usually also in the roster (its old core is still an active
    // machine until the operator removes it), so it must be filtered out of its
    // roster position first — otherwise it stays ahead of the promoted core and
    // retain_servers_order reconnects straight back onto it.
    let mut pool: Vec<String> = reachable_machine_urls(snapshot)
        .into_iter()
        .filter(|url| url != &seed)
        .collect();
    pool.push(seed);
    pool
}

/// Mirror core intent to the machine-local store off the drumbeat, so a future
/// promotion can seed a new core without a backup restore (ADR 0031). The
/// epoch-gating that drops a stale core's intent lives in the store.
///
/// The task re-subscribes on a failed or ended subscription rather than dying:
/// a mirror that quietly stops updating is the one failure this seam must make
/// impossible, since the mirror is what makes promotion instant.
fn start_intent_mirror(
    client: NatsClient,
    mirror: MachineIntentMirror,
    seed: NatsClientUrl,
) -> RunningTask {
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        // Allow the first stale-connection detection to enforce immediately, then
        // rate-limit to avoid a reconnect storm.
        let mut last_enforced = Instant::now()
            .checked_sub(EPOCH_ENFORCE_INTERVAL)
            .unwrap_or_else(Instant::now);
        loop {
            let mut changed = match client.subscribe(INTENT_CHANGED).await {
                Ok(subscription) => subscription,
                // Not connected yet, or a transient failure: back off and retry.
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
                        // Stream ended (a hard disconnect): re-subscribe.
                        let Some(message) = message else { break };
                        // Empty pings only say "something changed"; the periodic
                        // drumbeat carries the full snapshot the mirror persists.
                        if message.payload.is_empty() {
                            continue;
                        }
                        if let Ok(snapshot) =
                            serde_json::from_slice::<IntentSnapshot>(&message.payload)
                        {
                            match mirror.store(&snapshot) {
                                // Accepted (current or higher epoch): refresh the
                                // failover pool with the configured core plus each
                                // Reachable Machine. Updates the pool used on the
                                // *next* reconnect; never forces one, so the live
                                // core connection stays undisturbed.
                                Ok(true) => {
                                    let _ = client
                                        .set_server_pool(candidate_server_pool(&snapshot, &seed))
                                        .await;
                                }
                                // Rejected as stale: this drumbeat arrived on a
                                // connection to a healed old core advertising a lower
                                // epoch than one already seen. Deprioritize that stale
                                // seed to last in the pool (rebuilding from the
                                // highest-epoch snapshot we've stored, which carries
                                // the promoted core as a Reachable Machine) so
                                // retain_servers_order does not just reconnect us
                                // straight back onto it, then force the reconnect.
                                // Rate-limited so a persistent stale core can't cause a
                                // reconnect storm.
                                Ok(false) => {
                                    if last_enforced.elapsed() >= EPOCH_ENFORCE_INTERVAL {
                                        last_enforced = Instant::now();
                                        if let Some(best) = mirror.load() {
                                            let _ = client
                                                .set_server_pool(candidate_server_pool_seed_last(
                                                    &best, &seed,
                                                ))
                                                .await;
                                        }
                                        let _ = client.force_reconnect().await;
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    _ = &mut shutdown_rx => return,
                }
            }
        }
    });
    RunningTask { shutdown, task }
}

fn start_pending_join_mirror(client: NatsClient, mirror: MachinePendingJoinMirror) -> RunningTask {
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
    RunningTask { shutdown, task }
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
            let attempt = publisher
                .publish_with_timeout(&machine_id, &runner, MACHINE_OBSERVATION_TIMEOUT)
                .await;
            backoff = record_observer_attempt(&task_health, attempt, interval, backoff);
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = &mut shutdown_rx => break,
            }
        }
    });

    RunningTask { shutdown, task }
}

fn record_observer_attempt(
    health: &Mutex<MachineObserverHealth>,
    attempt: Result<(), MachineProcessError>,
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
        let endpoints = refresh_machine_endpoints(machine_id, &self.endpoint_cache).await;
        let facts = read_machine_facts_snapshot(machine_id, runner, endpoints, current_unix_ms())
            .await
            .map_err(MachineProcessError::ReadFacts)?;
        let payload = serde_json::to_vec(&facts).map_err(MachineProcessError::EncodeFacts)?;
        self.client
            .publish(machine_facts(machine_id), payload.into())
            .await
            .map_err(|error| MachineProcessError::PublishFacts {
                message: error.to_string(),
            })?;
        self.client
            .flush()
            .await
            .map_err(|error| MachineProcessError::PublishFacts {
                message: error.to_string(),
            })?;

        Ok(())
    }
}

pub async fn run_machine_until_shutdown(
    config: &MachineProcessConfig,
) -> Result<(), MachineProcessError> {
    let runtime = start_machine_process(config).await?;
    shutdown_signal()
        .await
        .map_err(MachineProcessError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(MachineProcessError::ShutdownMachineService)
}

#[derive(Debug)]
pub enum MachineProcessError {
    AwaitCredentials(AwaitSeedFileError),
    ConnectNats(NatsConnectError),
    ReadFacts(MachineFactsReadError),
    EncodeFacts(serde_json::Error),
    PublishFacts { message: String },
    ObservationTimedOut { timeout: Duration },
    InvalidDataplaneMtu { message: String },
    StartMachineService(MachineServiceError),
    ShutdownSignal(std::io::Error),
    ShutdownMachineService(NatsServiceShutdownError),
}

impl fmt::Display for MachineProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitCredentials(error) => write!(formatter, "{error}"),
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::ReadFacts(error) => write!(formatter, "failed to read machine facts: {error}"),
            Self::EncodeFacts(error) => {
                write!(formatter, "failed to encode machine facts: {error}")
            }
            Self::PublishFacts { message } => {
                write!(formatter, "failed to publish machine facts: {message}")
            }
            Self::ObservationTimedOut { timeout } => {
                write!(
                    formatter,
                    "machine observation publish timed out after {}s",
                    timeout.as_secs()
                )
            }
            Self::InvalidDataplaneMtu { message } => {
                write!(formatter, "invalid dataplane WireGuard MTU: {message}")
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

impl std::error::Error for MachineProcessError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::machine::runner::{
        CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
        MachineContainerRunner, MachineContainerRunnerError, MachineLogReader,
        MachineLogReaderError, MachineLogTail,
    };
    use crate::roles::machine::service::observation_state;
    use futures_util::StreamExt;
    use ployz_core::dataplane::{
        EbpfForwardingReady, EbpfForwardingReadyEvidence, PloyzNativeMeshReady,
        WireGuardEbpfPrepareError, WireGuardReady, WireGuardReadyEvidence,
    };
    use ployz_core::ids::{ContainerId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId};
    use ployz_core::machine_runtime::ManagedContainerKind;
    use ployz_core::machine_runtime::{
        ContainerRuntimeState, MachineFactsSnapshot, ManagedContainerIdentity,
    };
    use ployz_core::state::ActiveMachineState;
    use ployz_core::subjects::machine_facts;
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

    fn active_machine_with(id: &str, endpoint: Option<&str>) -> ActiveMachineState {
        ActiveMachineState {
            machine_id: machine_id(id),
            name: ployz_core::machine::MachineName::try_new(id).expect("machine name"),
            activated_by: ployz_test_support::ids::operation_id("op_activate"),
            lifecycle: ployz_core::state::MachineLifecycle::Active,
            control_endpoints: endpoint
                .map(|ip| vec![ip.parse().expect("ip")])
                .unwrap_or_default(),
            mesh_endpoints: Vec::new(),
        }
    }

    fn snapshot_with(machines: Vec<ActiveMachineState>) -> IntentSnapshot {
        IntentSnapshot {
            epoch: ployz_core::state::ControlPlaneEpoch::initial(),
            active_machines: machines,
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            authorized_users: Vec::new(),
        }
    }

    #[test]
    fn candidate_server_pool_keeps_the_seed_first_and_adds_reachable_machines() {
        let seed = NatsClientUrl::try_new("tls://10.0.0.1:4222").expect("seed url");
        let snapshot = snapshot_with(vec![
            active_machine_with("machine_a", Some("203.0.113.5")),
            active_machine_with("machine_b", None),
            active_machine_with("machine_c", Some("203.0.113.9")),
        ]);
        assert_eq!(
            candidate_server_pool(&snapshot, &seed),
            vec![
                "tls://10.0.0.1:4222".to_owned(),
                "tls://203.0.113.5:4222".to_owned(),
                "tls://203.0.113.9:4222".to_owned(),
            ]
        );
    }

    #[test]
    fn candidate_server_pool_seed_last_moves_a_rostered_stale_seed_to_the_end() {
        // The stale seed's old core is still an active machine in the roster, so its
        // URL is among the reachable machines. seed_last must drop it from that slot
        // and place it strictly last, behind the promoted core.
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");
        let snapshot = snapshot_with(vec![
            active_machine_with("old_core", Some("203.0.113.5")),
            active_machine_with("promoted_core", Some("203.0.113.9")),
        ]);
        assert_eq!(
            candidate_server_pool_seed_last(&snapshot, &seed),
            vec![
                "tls://203.0.113.9:4222".to_owned(),
                "tls://203.0.113.5:4222".to_owned(),
            ]
        );
    }

    #[test]
    fn candidate_server_pool_always_retains_the_seed_when_no_machine_is_reachable() {
        let seed = NatsClientUrl::try_new("tls://10.0.0.1:4222").expect("seed url");
        // The configured core must never be dropped: not for an empty roster, and
        // not when the only machines have advertised no endpoint yet.
        assert_eq!(
            candidate_server_pool(&snapshot_with(Vec::new()), &seed),
            vec!["tls://10.0.0.1:4222".to_owned()]
        );
        assert_eq!(
            candidate_server_pool(
                &snapshot_with(vec![active_machine_with("machine_a", None)]),
                &seed
            ),
            vec!["tls://10.0.0.1:4222".to_owned()]
        );
    }

    #[test]
    fn mirrored_server_pool_loads_reachable_machines_from_cached_intent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let seed_file = dir.path().join("machine.seed");
        let mirror = MachineIntentMirror::new(dir.path().join("intent-mirror.json"));
        mirror
            .store(&snapshot_with(vec![active_machine_with(
                "promoted_core",
                Some("203.0.113.9"),
            )]))
            .expect("store mirror");
        let seed = NatsClientUrl::try_new("tls://203.0.113.5:4222").expect("seed url");

        assert_eq!(
            mirrored_server_pool(&seed_file, &seed),
            vec![
                "tls://203.0.113.5:4222".to_owned(),
                "tls://203.0.113.9:4222".to_owned(),
            ]
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
            MachineIntentMirror::new(mirror_dir.path().join("intent-mirror.json")),
            MachinePendingJoinMirror::new(mirror_dir.path().join("pending-machine-joins.json")),
            NatsClientUrl::try_new("tls://127.0.0.1:4222").expect("seed url"),
            Duration::from_secs(60),
            "ployz-wg0".to_owned(),
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
