//! Process wiring for the gateway role.

use crate::adapters::credentials::{
    AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials,
};
use crate::config::GatewayProcessConfig;
use crate::control::intent::service::NatsIntentReader;
use crate::process_support::{
    BackoffSchedule, LazyHandle, RecordedAttempt, RefreshDelay, bounded_role_shutdown,
    drain_refresh_wakes, record_attempt, shutdown_signal, sleep_or_shutdown,
    wait_for_refresh_delay,
};
use crate::recovery::IntentMirror;
use crate::recovery::{IntentFailover, mirrored_server_pool, spawn_intent_failover_mirror};
use crate::role_testimony::{
    RoleTestimonyCache, RoleTestimonyCacheError, RunningRoleTestimonyCache,
    start_role_testimony_cache,
};
use crate::roles::gateway::pingora::{
    GatewayPingoraFailureRecorder, GatewayTlsAccept, PingoraRouteRegistry,
    PingoraRouteRegistryError, PloyzGatewayProxy,
};
use crate::roles::gateway::projection::{GatewayProjection, GatewayProjectionUpdate};
use crate::roles::gateway::route_table::{
    GatewayProjector, GatewayProjectorTick, GatewayServingState,
};
use crate::roles::gateway::source::GatewayCertificateStore;
use crate::roles::gateway::source::load_gateway_projection_update_from_nats;
use futures_util::StreamExt;
use pingora::listeners::tls::TlsSettings;
use pingora::server::configuration::ServerConf;
use pingora::server::{RunArgs, Server, ShutdownSignal, ShutdownSignalWatch};
use ployz_core::ids::MachineId;
use ployz_core::machine::GatewayServingStatus;
use ployz_core::machine::GatewayStatusObservation;
use ployz_core::operation::RoutePort;

use ployz_nats::connect::{NatsConnectError, connect_authenticated_pool};
use ployz_nats::service_runtime::{
    NatsClient, NatsServiceRuntimeError, NatsServiceShutdownError, RunningNatsService,
};
use ployz_nats::subjects::{INTENT_CHANGED, gateway_status, machine_facts_scope};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

const GATEWAY_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const GATEWAY_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const GATEWAY_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const GATEWAY_WATCH_RESTART_DELAY: Duration = Duration::from_secs(1);
const GATEWAY_HEALTH_CHECK_INTERVAL: Duration = Duration::from_millis(100);
const GATEWAY_LISTENER_READY_TIMEOUT: Duration = Duration::from_secs(2);
const GATEWAY_LISTENER_READY_POLL: Duration = Duration::from_millis(10);
const GATEWAY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(9);

mod service;

pub use service::start_gateway_certificate_service;
use service::start_gateway_role_service;

pub struct RunningGatewayProcess {
    runtime: Arc<Mutex<GatewayProjector>>,
    health: Arc<Mutex<GatewayProcessHealth>>,
    listen_addr: SocketAddr,
    shutdown: broadcast::Sender<()>,
    pingora_shutdown: watch::Sender<bool>,
    testimony_cache: RunningRoleTestimonyCache,
    gateway_service: RunningNatsService,
    _certificate_state_guard: Option<tempfile::TempDir>,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningGatewayProcess {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        let Self {
            shutdown,
            pingora_shutdown,
            testimony_cache,
            gateway_service,
            _certificate_state_guard,
            mut tasks,
            ..
        } = self;
        let _ = shutdown.send(());
        let _ = pingora_shutdown.send(true);
        let abort_handles = tasks
            .iter()
            .map(JoinHandle::abort_handle)
            .collect::<Vec<_>>();
        let cleanup = async {
            testimony_cache.shutdown().await;
            let service_shutdown = gateway_service.shutdown().await;
            for task in &mut tasks {
                let _ = task.await;
            }
            service_shutdown
        };
        bounded_role_shutdown("gateway", GATEWAY_SHUTDOWN_TIMEOUT, &abort_handles, cleanup).await
    }

    #[must_use]
    pub fn health(&self) -> GatewayProcessHealth {
        self.health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    #[must_use]
    pub fn served_projection(&self) -> Option<GatewayProjection> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .route_table()
            .current()
            .cloned()
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    #[must_use]
    pub fn tls_listen_addr(&self) -> SocketAddr {
        gateway_tls_listen_addr(self.listen_addr)
    }
}

pub async fn start_gateway_process(
    config: &GatewayProcessConfig,
) -> Result<RunningGatewayProcess, GatewayProcessError> {
    // Gateway authenticates as the machine's Machine user (no Gateway
    // principal in v1) and awaits the seed file like the machine role does.
    let connect = await_role_credentials(
        "gateway",
        &config.nats,
        &SeedFileRetryPolicy::default_policy(),
    )
    .await
    .map_err(GatewayProcessError::AwaitCredentials)?;
    let mirror = IntentMirror::beside_seed_file(&config.nats.seed_file);
    let pool = mirrored_server_pool(&mirror, &connect.url);
    let client = connect_authenticated_pool(&connect, &pool, GATEWAY_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(GatewayProcessError::ConnectNats)?;
    start_gateway_process_with_client_and_state_dir(
        client,
        GATEWAY_REFRESH_INTERVAL,
        config.listen_addr,
        config.machine_id.clone(),
        config.certificate_state_dir.clone(),
        Some(IntentFailover {
            mirror,
            seed: connect.url,
        }),
    )
    .await
}

pub async fn start_gateway_process_with_client(
    client: NatsClient,
    refresh_interval: Duration,
    listen_addr: SocketAddr,
    machine_id: MachineId,
    failover: Option<IntentFailover>,
) -> Result<RunningGatewayProcess, GatewayProcessError> {
    let certificate_state =
        tempfile::tempdir().map_err(GatewayProcessError::CreateIsolatedCertificateState)?;
    let certificate_state_dir = certificate_state.path().to_path_buf();
    start_gateway_process_inner(
        client,
        refresh_interval,
        listen_addr,
        machine_id,
        certificate_state_dir,
        failover,
        Some(certificate_state),
    )
    .await
}

pub async fn start_gateway_process_with_client_and_state_dir(
    client: NatsClient,
    refresh_interval: Duration,
    listen_addr: SocketAddr,
    machine_id: MachineId,
    certificate_state_dir: PathBuf,
    failover: Option<IntentFailover>,
) -> Result<RunningGatewayProcess, GatewayProcessError> {
    start_gateway_process_inner(
        client,
        refresh_interval,
        listen_addr,
        machine_id,
        certificate_state_dir,
        failover,
        None,
    )
    .await
}

async fn start_gateway_process_inner(
    client: NatsClient,
    refresh_interval: Duration,
    listen_addr: SocketAddr,
    machine_id: MachineId,
    certificate_state_dir: PathBuf,
    failover: Option<IntentFailover>,
    certificate_state_guard: Option<tempfile::TempDir>,
) -> Result<RunningGatewayProcess, GatewayProcessError> {
    let listen_addr = resolve_gateway_listen_addr(listen_addr).await?;
    let listener_port =
        RoutePort::try_new(listen_addr.port()).expect("bound TCP listener port is non-zero");
    let runtime = Arc::new(Mutex::new(GatewayProjector::new()));
    let registry = PingoraRouteRegistry::new();
    let certificate_store = GatewayCertificateStore::new(certificate_state_dir);
    let testimony_cache = start_role_testimony_cache(client.clone())
        .await
        .map_err(GatewayProcessError::StartFactsCache)?;
    let gateway_service = match start_gateway_role_service(
        client.clone(),
        machine_id.clone(),
        certificate_store.clone(),
        registry.clone(),
        Arc::clone(&runtime),
        listen_addr,
    )
    .await
    {
        Ok(service) => service,
        Err(error) => {
            testimony_cache.shutdown().await;
            return Err(GatewayProcessError::StartCertificateService(error));
        }
    };
    let facts = testimony_cache.cache();
    let health = Arc::new(Mutex::new(GatewayProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
        last_http_failure: None,
        consecutive_http_failures: 0,
        last_watch_failure: None,
        consecutive_watch_failures: 0,
        last_status_publish_failure: None,
        consecutive_status_publish_failures: 0,
    }));
    let (shutdown, _) = broadcast::channel(2);
    let mut tasks = Vec::new();
    if let Some(failover) = failover {
        tasks.push(spawn_intent_failover_mirror(
            client.clone(),
            failover,
            shutdown.subscribe(),
        ));
    }
    let (pingora_shutdown, pingora_shutdown_rx) = watch::channel(false);
    let pingora_health = Arc::clone(&health);
    let pingora_registry = registry.clone();
    let http_task = tokio::task::spawn_blocking(move || {
        run_pingora_gateway_server(
            listen_addr,
            listener_port,
            pingora_registry,
            pingora_health,
            pingora_shutdown_rx,
        );
    });
    if let Err(error) = wait_for_gateway_listener_ready(listen_addr, &http_task).await {
        let _ = pingora_shutdown.send(true);
        let _ = http_task.await;
        let _ = gateway_service.shutdown().await;
        testimony_cache.shutdown().await;
        return Err(error);
    }
    let tls_addr = gateway_tls_listen_addr(listen_addr);
    if tls_addr.port() != 0
        && let Err(error) = wait_for_gateway_listener_ready(tls_addr, &http_task).await
    {
        let _ = pingora_shutdown.send(true);
        let _ = http_task.await;
        let _ = gateway_service.shutdown().await;
        testimony_cache.shutdown().await;
        return Err(error);
    }

    let (refresh_wake, refresh_wake_rx) = mpsc::channel(1);
    let task_runtime = Arc::clone(&runtime);
    let task_registry = registry.clone();
    let task_health = Arc::clone(&health);
    let mut refresh_shutdown = shutdown.subscribe();
    let refresh_client = client.clone();
    let refresh_facts = facts.clone();
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let mut source =
            GatewayProcessSource::new(refresh_client, refresh_facts, certificate_store);
        let mut refresh_wake_rx = refresh_wake_rx;

        loop {
            drain_refresh_wakes(&mut refresh_wake_rx);
            let attempt = tokio::select! {
                attempt = source.refresh_with_timeout(&task_runtime, &task_registry) => attempt,
                _ = refresh_shutdown.recv() => break,
            };
            let observed = gateway_observation_from_attempt(&machine_id, listen_addr, &attempt);
            backoff = record_gateway_attempt(&task_health, attempt, refresh_interval, backoff);
            let status_publish = tokio::select! {
                result = source.replace_gateway_status(&observed) => result,
                _ = refresh_shutdown.recv() => break,
            };
            record_gateway_status_publish_result(&task_health, status_publish);

            match wait_for_refresh_delay(backoff, &mut refresh_wake_rx, &mut refresh_shutdown).await
            {
                RefreshDelay::Elapsed | RefreshDelay::Woken => {}
                RefreshDelay::WakeClosed | RefreshDelay::Shutdown => break,
            }
        }
    });
    let watch_client = client.clone();
    let watch_health = Arc::clone(&health);
    let mut watch_shutdown = shutdown.subscribe();
    let watch_task = tokio::spawn(async move {
        wake_gateway_refresh_on_nats_changes(
            watch_client,
            refresh_wake,
            watch_health,
            &mut watch_shutdown,
        )
        .await;
    });
    let health_registry = registry.clone();
    let mut health_shutdown = shutdown.subscribe();
    let health_task = tokio::spawn(async move {
        run_gateway_health_checks(health_registry, &mut health_shutdown).await;
    });

    tasks.push(refresh_task);
    tasks.push(watch_task);
    tasks.push(http_task);
    tasks.push(health_task);

    Ok(RunningGatewayProcess {
        runtime,
        health,
        listen_addr,
        shutdown,
        pingora_shutdown,
        testimony_cache,
        gateway_service,
        _certificate_state_guard: certificate_state_guard,
        tasks,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessHealth {
    pub last_attempt: Option<GatewayProcessAttempt>,
    pub consecutive_failures: u64,
    pub last_http_failure: Option<GatewayHttpFailure>,
    pub consecutive_http_failures: u64,
    pub last_watch_failure: Option<GatewayWatchFailure>,
    pub consecutive_watch_failures: u64,
    pub last_status_publish_failure: Option<GatewayStatusPublishFailure>,
    pub consecutive_status_publish_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayProcessAttempt {
    Current { route_count: usize },
    ServingLastKnownGood { route_count: usize, message: String },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayHttpFailure {
    Proxy { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayWatchFailure {
    Open { message: String },
    Ended { source: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayStatusPublishFailure {
    Write { message: String },
}

struct GatewayProcessSource {
    client: NatsClient,
    facts: RoleTestimonyCache,
    stores: LazyHandle<GatewayProcessStores>,
    certificate_store: GatewayCertificateStore,
}

impl GatewayProcessSource {
    fn new(
        client: NatsClient,
        facts: RoleTestimonyCache,
        certificate_store: GatewayCertificateStore,
    ) -> Self {
        Self {
            client,
            facts,
            stores: LazyHandle::new(),
            certificate_store,
        }
    }

    async fn refresh_with_timeout(
        &mut self,
        runtime: &Mutex<GatewayProjector>,
        registry: &PingoraRouteRegistry,
    ) -> Result<GatewayProjectorTick, GatewayProcessError> {
        tokio::time::timeout(GATEWAY_REFRESH_TIMEOUT, self.refresh(runtime, registry))
            .await
            .map_err(|_| GatewayProcessError::RefreshTimedOut {
                timeout: GATEWAY_REFRESH_TIMEOUT,
            })?
    }

    async fn refresh(
        &mut self,
        runtime: &Mutex<GatewayProjector>,
        registry: &PingoraRouteRegistry,
    ) -> Result<GatewayProjectorTick, GatewayProcessError> {
        let update = self.load_update().await?;
        let mut runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut candidate = runtime.clone();
        let tick = candidate.apply_source_update(update);
        if let Some(projection) = tick.served.as_ref() {
            registry
                .replace_projection(projection)
                .map_err(GatewayProcessError::UpdatePingoraRoutes)?;
        }
        *runtime = candidate;

        Ok(tick)
    }

    async fn load_update(&mut self) -> Result<GatewayProjectionUpdate, GatewayProcessError> {
        let facts = self.facts.clone();
        let certificate_store = self.certificate_store.clone();
        let stores = self.stores().await?;
        Ok(load_gateway_projection_update_from_nats(
            &stores.intent_reader,
            &facts,
            &certificate_store,
        )
        .await)
    }

    async fn replace_gateway_status(
        &mut self,
        status: &GatewayStatusObservation,
    ) -> Result<(), GatewayProcessError> {
        let payload =
            serde_json::to_vec(status).map_err(GatewayProcessError::EncodeGatewayStatus)?;
        self.client
            .publish(gateway_status(&status.machine_id), payload.into())
            .await
            .map_err(|error| GatewayProcessError::PublishGatewayStatus {
                message: error.to_string(),
            })?;
        self.client
            .flush()
            .await
            .map_err(|error| GatewayProcessError::PublishGatewayStatus {
                message: error.to_string(),
            })?;
        self.facts.record_gateway_status(status.clone());
        Ok(())
    }

    async fn stores(&mut self) -> Result<&GatewayProcessStores, GatewayProcessError> {
        let client = &self.client;
        self.stores
            .get_or_open(async || open_gateway_process_stores(client.clone()).await)
            .await
    }
}

struct GatewayProcessStores {
    intent_reader: NatsIntentReader,
}

async fn open_gateway_process_stores(
    client: NatsClient,
) -> Result<GatewayProcessStores, GatewayProcessError> {
    Ok(GatewayProcessStores {
        intent_reader: NatsIntentReader::new(client),
    })
}

async fn wake_gateway_refresh_on_nats_changes(
    client: NatsClient,
    refresh_wake: mpsc::Sender<()>,
    health: Arc<Mutex<GatewayProcessHealth>>,
    shutdown: &mut broadcast::Receiver<()>,
) {
    loop {
        let opened = tokio::select! {
            opened = open_gateway_change_watchers(client.clone()) => opened,
            _ = shutdown.recv() => break,
        };
        match opened {
            Ok(mut watchers) => {
                let _ = refresh_wake.try_send(());
                match watch_gateway_changes(&mut watchers, &refresh_wake, &health, shutdown).await {
                    GatewayWatchLoopEnd::Shutdown => break,
                    GatewayWatchLoopEnd::Restart => {}
                }
            }
            Err(error) => {
                record_gateway_watch_failure(
                    &health,
                    GatewayWatchFailure::Open {
                        message: error.to_string(),
                    },
                );
                if sleep_or_shutdown(GATEWAY_WATCH_RESTART_DELAY, shutdown).await {
                    break;
                }
            }
        }
    }
}

struct GatewayChangeWatchers {
    intent: async_nats::Subscriber,
    machine_facts: async_nats::Subscriber,
}

async fn open_gateway_change_watchers(
    client: NatsClient,
) -> Result<GatewayChangeWatchers, GatewayProcessError> {
    let intent = client.subscribe(INTENT_CHANGED).await.map_err(|error| {
        GatewayProcessError::WatchIntent {
            message: error.to_string(),
        }
    })?;
    let subject = machine_facts_scope();
    let machine_facts = client.subscribe(subject.clone()).await.map_err(|error| {
        GatewayProcessError::WatchFacts {
            subject,
            message: error.to_string(),
        }
    })?;

    Ok(GatewayChangeWatchers {
        intent,
        machine_facts,
    })
}

async fn watch_gateway_changes(
    watchers: &mut GatewayChangeWatchers,
    refresh_wake: &mpsc::Sender<()>,
    health: &Mutex<GatewayProcessHealth>,
    shutdown: &mut broadcast::Receiver<()>,
) -> GatewayWatchLoopEnd {
    loop {
        tokio::select! {
            intent = watchers.intent.next() => {
                match watch_intent_change(intent) {
                    GatewayWatchEvent::Changed => {
                        record_gateway_watch_success(health);
                        let _ = refresh_wake.try_send(());
                    }
                    GatewayWatchEvent::Failed(failure) => {
                        record_gateway_watch_failure(health, failure);
                        return GatewayWatchLoopEnd::Restart;
                    }
                }
            }
            facts = watchers.machine_facts.next() => {
                match watch_plain_change("machine facts", facts) {
                    GatewayWatchEvent::Changed => {
                        record_gateway_watch_success(health);
                        let _ = refresh_wake.try_send(());
                    }
                    GatewayWatchEvent::Failed(failure) => {
                        record_gateway_watch_failure(health, failure);
                        return GatewayWatchLoopEnd::Restart;
                    }
                }
            }
            _ = shutdown.recv() => return GatewayWatchLoopEnd::Shutdown,
        }
    }
}

fn watch_intent_change(event: Option<async_nats::Message>) -> GatewayWatchEvent {
    watch_plain_change("intent", event)
}

fn watch_plain_change(
    source: &'static str,
    event: Option<async_nats::Message>,
) -> GatewayWatchEvent {
    match event {
        Some(_) => GatewayWatchEvent::Changed,
        None => GatewayWatchEvent::Failed(GatewayWatchFailure::Ended { source }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayWatchLoopEnd {
    Restart,
    Shutdown,
}

enum GatewayWatchEvent {
    Changed,
    Failed(GatewayWatchFailure),
}

fn record_gateway_watch_success(health: &Mutex<GatewayProcessHealth>) {
    let mut health = health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    health.last_watch_failure = None;
    health.consecutive_watch_failures = 0;
}

fn record_gateway_watch_failure(
    health: &Mutex<GatewayProcessHealth>,
    failure: GatewayWatchFailure,
) {
    let mut health = health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    health.last_watch_failure = Some(failure);
    health.consecutive_watch_failures += 1;
}

fn record_gateway_status_publish_result(
    health: &Mutex<GatewayProcessHealth>,
    result: Result<(), GatewayProcessError>,
) {
    let mut health = health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match result {
        Ok(()) => {
            health.last_status_publish_failure = None;
            health.consecutive_status_publish_failures = 0;
        }
        Err(error) => {
            health.last_status_publish_failure = Some(GatewayStatusPublishFailure::Write {
                message: error.to_string(),
            });
            health.consecutive_status_publish_failures += 1;
        }
    }
}

fn record_gateway_attempt(
    health: &Mutex<GatewayProcessHealth>,
    attempt: Result<GatewayProjectorTick, GatewayProcessError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let GatewayProcessHealth {
        last_attempt,
        consecutive_failures,
        ..
    } = &mut *health;
    let recorded = match attempt {
        Ok(tick) => {
            let attempt = gateway_attempt_from_tick(tick);
            if matches!(attempt, GatewayProcessAttempt::Current { .. }) {
                RecordedAttempt::Healthy(attempt)
            } else {
                // Last-known-good serving counts as a failure streak but
                // keeps the steady refresh interval.
                RecordedAttempt::Degraded(attempt)
            }
        }
        Err(error) => RecordedAttempt::Failed(GatewayProcessAttempt::Failed {
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

fn gateway_attempt_from_tick(tick: GatewayProjectorTick) -> GatewayProcessAttempt {
    match tick.serving {
        GatewayServingState::Current { route_count } => {
            GatewayProcessAttempt::Current { route_count }
        }
        GatewayServingState::LastKnownGood { route_count, error } => {
            GatewayProcessAttempt::ServingLastKnownGood {
                route_count,
                message: format!("{error:?}"),
            }
        }
        GatewayServingState::Unavailable { error } => GatewayProcessAttempt::Failed {
            message: error
                .map(|error| format!("{error:?}"))
                .unwrap_or_else(|| "gateway source unavailable".to_owned()),
        },
    }
}

fn gateway_observation_from_attempt(
    machine_id: &MachineId,
    listen_addr: SocketAddr,
    attempt: &Result<GatewayProjectorTick, GatewayProcessError>,
) -> GatewayStatusObservation {
    match attempt {
        Ok(tick) => match &tick.serving {
            GatewayServingState::Current { route_count } => GatewayStatusObservation {
                machine_id: machine_id.clone(),
                listen_addr,
                serving: GatewayServingStatus::Current,
                route_count: *route_count,
            },
            GatewayServingState::LastKnownGood { route_count, .. } => GatewayStatusObservation {
                machine_id: machine_id.clone(),
                listen_addr,
                serving: GatewayServingStatus::LastKnownGood,
                route_count: *route_count,
            },
            GatewayServingState::Unavailable { .. } => GatewayStatusObservation {
                machine_id: machine_id.clone(),
                listen_addr,
                serving: GatewayServingStatus::Unavailable,
                route_count: 0,
            },
        },
        Err(_) => GatewayStatusObservation {
            machine_id: machine_id.clone(),
            listen_addr,
            serving: GatewayServingStatus::Unavailable,
            route_count: 0,
        },
    }
}

fn record_gateway_http_failure(health: &Mutex<GatewayProcessHealth>, failure: GatewayHttpFailure) {
    let mut health = health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    health.last_http_failure = Some(failure);
    health.consecutive_http_failures += 1;
}

async fn resolve_gateway_listen_addr(
    listen_addr: SocketAddr,
) -> Result<SocketAddr, GatewayProcessError> {
    let listener =
        TcpListener::bind(listen_addr)
            .await
            .map_err(|source| GatewayProcessError::BindHttp {
                addr: listen_addr,
                source,
            })?;
    let resolved = listener
        .local_addr()
        .map_err(GatewayProcessError::ReadHttpListenerAddr)?;
    drop(listener);
    Ok(resolved)
}

async fn wait_for_gateway_listener_ready(
    listen_addr: SocketAddr,
    http_task: &JoinHandle<()>,
) -> Result<(), GatewayProcessError> {
    let deadline = tokio::time::Instant::now() + GATEWAY_LISTENER_READY_TIMEOUT;
    loop {
        if tokio::time::timeout(GATEWAY_LISTENER_READY_POLL, TcpStream::connect(listen_addr))
            .await
            .is_ok_and(|connected| connected.is_ok())
        {
            return Ok(());
        }
        if http_task.is_finished() || tokio::time::Instant::now() >= deadline {
            return Err(GatewayProcessError::HttpListenerNotReady {
                addr: listen_addr,
                timeout: GATEWAY_LISTENER_READY_TIMEOUT,
            });
        }
        tokio::time::sleep(GATEWAY_LISTENER_READY_POLL).await;
    }
}

fn run_pingora_gateway_server(
    listen_addr: SocketAddr,
    listener_port: RoutePort,
    registry: PingoraRouteRegistry,
    health: Arc<Mutex<GatewayProcessHealth>>,
    shutdown: watch::Receiver<bool>,
) {
    let conf = ServerConf {
        grace_period_seconds: Some(0),
        graceful_shutdown_timeout_seconds: Some(1),
        ..Default::default()
    };

    let mut server = Server::new_with_opt_and_conf(None, conf);
    server.bootstrap();
    let recorder: GatewayPingoraFailureRecorder = Arc::new(move |failure| {
        record_gateway_http_failure(&health, GatewayHttpFailure::Proxy { message: failure });
    });
    let proxy = PloyzGatewayProxy::redirecting_managed_http(
        registry.clone(),
        listener_port,
        Arc::clone(&recorder),
    );
    let mut http_service = pingora::proxy::http_proxy_service(&server.configuration, proxy);
    http_service.add_tcp(&listen_addr.to_string());
    server.add_service(http_service);

    let tls_addr = gateway_tls_listen_addr(listen_addr);
    let tls_proxy = PloyzGatewayProxy::new(
        registry.clone(),
        RoutePort::try_new(443).expect("HTTPS listener port is non-zero"),
        recorder,
    );
    let mut tls_service = pingora::proxy::http_proxy_service(&server.configuration, tls_proxy);
    let settings = TlsSettings::with_callbacks(Box::new(GatewayTlsAccept::new(&registry)))
        .expect("dynamic TLS settings are valid");
    tls_service.add_tls_with_settings(&tls_addr.to_string(), None, settings);
    server.add_service(tls_service);
    server.run(RunArgs {
        shutdown_signal: Box::new(GatewayPingoraShutdown { shutdown }),
    });
}

fn gateway_tls_listen_addr(http: SocketAddr) -> SocketAddr {
    let port = if http.port() == 80 {
        443
    } else {
        http.port().checked_add(363).unwrap_or(0)
    };
    SocketAddr::new(http.ip(), port)
}

struct GatewayPingoraShutdown {
    shutdown: watch::Receiver<bool>,
}

#[async_trait::async_trait]
impl ShutdownSignalWatch for GatewayPingoraShutdown {
    async fn recv(&self) -> ShutdownSignal {
        let mut shutdown = self.shutdown.clone();
        if *shutdown.borrow() {
            return ShutdownSignal::GracefulTerminate;
        }
        let _ = shutdown.changed().await;
        ShutdownSignal::GracefulTerminate
    }
}

async fn run_gateway_health_checks(
    registry: PingoraRouteRegistry,
    shutdown: &mut broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(GATEWAY_HEALTH_CHECK_INTERVAL) => {
                registry.run_health_checks().await;
            }
            _ = shutdown.recv() => break,
        }
    }
}

pub async fn run_gateway_until_shutdown(
    config: &GatewayProcessConfig,
) -> Result<(), GatewayProcessError> {
    let shutdown = shutdown_signal().map_err(GatewayProcessError::ShutdownSignal)?;
    let runtime = start_gateway_process(config).await?;
    shutdown
        .await
        .map_err(GatewayProcessError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(GatewayProcessError::ShutdownGatewayService)
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayProcessError {
    #[error("{0}")]
    AwaitCredentials(AwaitSeedFileError),
    #[error("{0}")]
    ConnectNats(NatsConnectError),
    #[error("failed to bind gateway HTTP listener {addr}: {source}")]
    BindHttp {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("failed to read gateway HTTP listener address: {0}")]
    ReadHttpListenerAddr(std::io::Error),
    #[error("gateway HTTP listener {addr} was not ready after {}s", timeout.as_secs())]
    HttpListenerNotReady { addr: SocketAddr, timeout: Duration },
    #[error("failed to start role testimony cache: {0}")]
    StartFactsCache(RoleTestimonyCacheError),
    #[error("failed to start gateway certificate service: {0}")]
    StartCertificateService(NatsServiceRuntimeError),
    #[error("failed to create isolated gateway certificate state: {0}")]
    CreateIsolatedCertificateState(std::io::Error),
    #[error("failed to encode gateway status: {0}")]
    EncodeGatewayStatus(serde_json::Error),
    #[error("failed to publish gateway status: {message}")]
    PublishGatewayStatus { message: String },
    #[error("failed to update Pingora gateway routes: {0}")]
    UpdatePingoraRoutes(PingoraRouteRegistryError),
    #[error("failed to watch intent: {message}")]
    WatchIntent { message: String },
    #[error("failed to watch {subject}: {message}")]
    WatchFacts { subject: String, message: String },
    #[error("gateway projection refresh timed out after {}s", timeout.as_secs())]
    RefreshTimedOut { timeout: Duration },
    #[error("failed to wait for shutdown: {0}")]
    ShutdownSignal(std::io::Error),
    #[error("failed to stop gateway service: {0:?}")]
    ShutdownGatewayService(NatsServiceShutdownError),
}

#[cfg(test)]
mod tests;
