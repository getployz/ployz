//! Process wiring for the gateway role.

use crate::adapters::credentials::{
    AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials,
};
use crate::config::GatewayProcessConfig;
use crate::fact_cache::{FactCache, FactCacheError, RunningFactCache, start_fact_cache};
use crate::intent::service::NatsIntentReader;
use crate::process_support::{
    BackoffSchedule, LazyHandle, RecordedAttempt, RefreshDelay, drain_refresh_wakes,
    record_attempt, shutdown_signal, sleep_or_shutdown, wait_for_refresh_delay,
};
use crate::roles::gateway::pingora::{
    GatewayPingoraFailureRecorder, PingoraRouteRegistry, PingoraRouteRegistryError,
    PloyzGatewayProxy,
};
use crate::roles::gateway::projection::{GatewayProjection, GatewayProjectionUpdate};
use crate::roles::gateway::route_table::{
    GatewayProjector, GatewayProjectorTick, GatewayServingState,
};
use crate::roles::gateway::source::load_gateway_projection_update_from_nats;
use crate::roles::machine::intent_mirror::MachineIntentMirror;
use crate::roles::nats_failover::{
    IntentFailover, mirrored_server_pool, spawn_intent_failover_mirror,
};
use futures_util::StreamExt;
use pingora::server::configuration::ServerConf;
use pingora::server::{RunArgs, Server, ShutdownSignal, ShutdownSignalWatch};
use ployz_core::ids::MachineId;
use ployz_core::ops::RoutePort;
use ployz_core::state::{GatewayServingStatus, GatewayStatusObservation};
use ployz_core::subjects::{INTENT_CHANGED, gateway_status, machine_facts_scope};
use ployz_nats::connect::{NatsConnectError, connect_authenticated_pool};
use ployz_nats::service_runtime::NatsClient;
use std::net::SocketAddr;
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

pub struct RunningGatewayProcess {
    runtime: Arc<Mutex<GatewayProjector>>,
    health: Arc<Mutex<GatewayProcessHealth>>,
    listen_addr: SocketAddr,
    shutdown: broadcast::Sender<()>,
    pingora_shutdown: watch::Sender<bool>,
    facts_cache: RunningFactCache,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningGatewayProcess {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.pingora_shutdown.send(true);
        for task in self.tasks {
            let _ = task.await;
        }
        self.facts_cache.shutdown().await;
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
    let mirror = MachineIntentMirror::beside_seed_file(&config.nats.seed_file);
    let pool = mirrored_server_pool(&mirror, &connect.url);
    let client = connect_authenticated_pool(&connect, &pool, GATEWAY_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(GatewayProcessError::ConnectNats)?;
    start_gateway_process_with_client(
        client,
        GATEWAY_REFRESH_INTERVAL,
        config.listen_addr,
        config.machine_id.clone(),
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
    let listen_addr = resolve_gateway_listen_addr(listen_addr).await?;
    let listener_port =
        RoutePort::try_new(listen_addr.port()).expect("bound TCP listener port is non-zero");
    let runtime = Arc::new(Mutex::new(GatewayProjector::new()));
    let registry = PingoraRouteRegistry::new();
    let facts_cache = start_fact_cache(client.clone())
        .await
        .map_err(GatewayProcessError::StartFactsCache)?;
    let facts = facts_cache.cache();
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
        let mut source = GatewayProcessSource::new(refresh_client, refresh_facts);
        let mut refresh_wake_rx = refresh_wake_rx;

        loop {
            drain_refresh_wakes(&mut refresh_wake_rx);
            let attempt = source
                .refresh_with_timeout(&task_runtime, &task_registry)
                .await;
            let observed = gateway_observation_from_attempt(&machine_id, listen_addr, &attempt);
            backoff = record_gateway_attempt(&task_health, attempt, refresh_interval, backoff);
            record_gateway_status_publish_result(
                &task_health,
                source.replace_gateway_status(&observed).await,
            );

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
        facts_cache,
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
    Accept { message: String },
    Proxy { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayWatchFailure {
    Open { message: String },
    Stream { message: String },
    Ended { source: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayStatusPublishFailure {
    Write { message: String },
}

struct GatewayProcessSource {
    client: NatsClient,
    facts: FactCache,
    stores: LazyHandle<GatewayProcessStores>,
}

impl GatewayProcessSource {
    fn new(client: NatsClient, facts: FactCache) -> Self {
        Self {
            client,
            facts,
            stores: LazyHandle::new(),
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
        let tick = {
            let mut runtime = runtime
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            runtime.apply_source_update(update)
        };
        if let Some(projection) = tick.served.as_ref() {
            registry
                .replace_projection(projection)
                .map_err(GatewayProcessError::UpdatePingoraRoutes)?;
        }

        Ok(tick)
    }

    async fn load_update(&mut self) -> Result<GatewayProjectionUpdate, GatewayProcessError> {
        let facts = self.facts.clone();
        let stores = self.stores().await?;
        Ok(load_gateway_projection_update_from_nats(&stores.intent_reader, &facts).await)
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
    let proxy = PloyzGatewayProxy::new(registry, listener_port, recorder);
    let mut service = pingora::proxy::http_proxy_service(&server.configuration, proxy);
    service.add_tcp(&listen_addr.to_string());
    server.add_service(service);
    server.run(RunArgs {
        shutdown_signal: Box::new(GatewayPingoraShutdown { shutdown }),
    });
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
    let runtime = start_gateway_process(config).await?;
    shutdown_signal()
        .await
        .map_err(GatewayProcessError::ShutdownSignal)?;
    runtime.shutdown().await;
    Ok(())
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
    #[error("failed to start runtime facts cache: {0}")]
    StartFactsCache(FactCacheError),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::gateway::projection::GatewayProjectionError;
    use crate::roles::gateway::route_table::GatewayServingState;

    #[test]
    fn retained_last_good_attempt_keeps_steady_refresh_interval() {
        let health = Mutex::new(GatewayProcessHealth {
            last_attempt: None,
            consecutive_failures: 0,
            last_http_failure: None,
            consecutive_http_failures: 0,
            last_watch_failure: None,
            consecutive_watch_failures: 0,
            last_status_publish_failure: None,
            consecutive_status_publish_failures: 0,
        });
        let interval = Duration::from_secs(1);

        let next = record_gateway_attempt(
            &health,
            Ok(GatewayProjectorTick {
                state: crate::roles::gateway::projection::GatewayProjectionState {
                    last_good: None,
                    last_error: Some(GatewayProjectionError::SourceUnavailable {
                        message: "not used".to_owned(),
                    }),
                },
                served: None,
                serving: GatewayServingState::LastKnownGood {
                    route_count: 1,
                    error: GatewayProjectionError::SourceUnavailable {
                        message: "nats unavailable".to_owned(),
                    },
                },
            }),
            interval,
            Duration::from_secs(30),
        );

        assert_eq!(next, interval);
        assert_eq!(
            health
                .lock()
                .expect("gateway health lock is not poisoned")
                .last_attempt,
            Some(GatewayProcessAttempt::ServingLastKnownGood {
                route_count: 1,
                message: "SourceUnavailable { message: \"nats unavailable\" }".to_owned(),
            })
        );
    }

    #[test]
    fn refresh_runtime_error_uses_exponential_backoff() {
        let health = Mutex::new(GatewayProcessHealth {
            last_attempt: None,
            consecutive_failures: 0,
            last_http_failure: None,
            consecutive_http_failures: 0,
            last_watch_failure: None,
            consecutive_watch_failures: 0,
            last_status_publish_failure: None,
            consecutive_status_publish_failures: 0,
        });

        let next = record_gateway_attempt(
            &health,
            Err(GatewayProcessError::RefreshTimedOut {
                timeout: Duration::from_secs(5),
            }),
            Duration::from_secs(1),
            Duration::from_secs(2),
        );

        assert_eq!(next, Duration::from_secs(4));
    }

    #[tokio::test]
    async fn pingora_shutdown_observes_signal_sent_before_recv() {
        let (shutdown, receiver) = watch::channel(false);
        shutdown.send(true).expect("shutdown signal sends");
        let shutdown = GatewayPingoraShutdown { shutdown: receiver };

        assert!(matches!(
            shutdown.recv().await,
            ShutdownSignal::GracefulTerminate
        ));
    }
}
