//! Runtime wiring for the gateway role.

use crate::config::GatewayProcessConfig;
use crate::gateway::{GatewayProjection, GatewayProjectionUpdate};
use crate::gateway_pingora::{
    GatewayPingoraFailureRecorder, PingoraRouteRegistry, PingoraRouteRegistryError,
    PloyzGatewayProxy,
};
use crate::gateway_runtime::{GatewayRuntime, GatewayRuntimeTick, GatewayServingState};
use crate::gateway_source::load_gateway_projection_update_from_nats;
use crate::machine_credentials::{AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials};
use crate::process_support::{
    BackoffSchedule, LazyHandle, RecordedAttempt, RefreshDelay, drain_refresh_wakes,
    record_attempt, shutdown_signal, sleep_or_shutdown, wait_for_refresh_delay,
};
use futures_util::StreamExt;
use pingora::server::configuration::ServerConf;
use pingora::server::{RunArgs, Server, ShutdownSignal, ShutdownSignalWatch};
use ployz_core::ids::MachineId;
use ployz_core::ops::RoutePort;
use ployz_core::state::{GatewayServingStatus, GatewayStatusObservation};
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::service_runtime::NatsClient;
use std::fmt;
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

pub struct RunningGatewayProcessRuntime {
    runtime: Arc<Mutex<GatewayRuntime>>,
    health: Arc<Mutex<GatewayProcessHealth>>,
    listen_addr: SocketAddr,
    shutdown: broadcast::Sender<()>,
    pingora_shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningGatewayProcessRuntime {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.pingora_shutdown.send(true);
        for task in self.tasks {
            let _ = task.await;
        }
    }

    #[must_use]
    pub fn health(&self) -> GatewayProcessHealth {
        self.health
            .lock()
            .expect("gateway health lock is not poisoned")
            .clone()
    }

    #[must_use]
    pub fn served_projection(&self) -> Option<GatewayProjection> {
        self.runtime
            .lock()
            .expect("gateway runtime lock is not poisoned")
            .route_table()
            .current()
            .cloned()
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }
}

pub async fn start_gateway_process_runtime(
    config: &GatewayProcessConfig,
) -> Result<RunningGatewayProcessRuntime, GatewayProcessRuntimeError> {
    // Gateway authenticates as the machine's Machine user (no Gateway
    // principal in v1) and awaits the seed file like the machine role does.
    let connect = await_role_credentials(
        "gateway",
        &config.nats,
        &SeedFileRetryPolicy::default_policy(),
    )
    .await
    .map_err(GatewayProcessRuntimeError::AwaitCredentials)?;
    let client = connect_authenticated(&connect, GATEWAY_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(GatewayProcessRuntimeError::ConnectNats)?;
    start_gateway_process_runtime_with_client(
        client,
        GATEWAY_REFRESH_INTERVAL,
        config.listen_addr,
        config.machine_id.clone(),
    )
    .await
}

pub async fn start_gateway_process_runtime_with_client(
    client: NatsClient,
    refresh_interval: Duration,
    listen_addr: SocketAddr,
    machine_id: MachineId,
) -> Result<RunningGatewayProcessRuntime, GatewayProcessRuntimeError> {
    let listen_addr = resolve_gateway_listen_addr(listen_addr).await?;
    let listener_port =
        RoutePort::try_new(listen_addr.port()).expect("bound TCP listener port is non-zero");
    let runtime = Arc::new(Mutex::new(GatewayRuntime::new()));
    let registry = PingoraRouteRegistry::new();
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
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let mut source = GatewayProcessSource::new(refresh_client);
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

    Ok(RunningGatewayProcessRuntime {
        runtime,
        health,
        listen_addr,
        shutdown,
        pingora_shutdown,
        tasks: vec![refresh_task, watch_task, http_task, health_task],
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
    stores: LazyHandle<GatewayProcessStores>,
}

impl GatewayProcessSource {
    fn new(client: NatsClient) -> Self {
        Self {
            client,
            stores: LazyHandle::new(),
        }
    }

    async fn refresh_with_timeout(
        &mut self,
        runtime: &Mutex<GatewayRuntime>,
        registry: &PingoraRouteRegistry,
    ) -> Result<GatewayRuntimeTick, GatewayProcessRuntimeError> {
        tokio::time::timeout(GATEWAY_REFRESH_TIMEOUT, self.refresh(runtime, registry))
            .await
            .map_err(|_| GatewayProcessRuntimeError::RefreshTimedOut {
                timeout: GATEWAY_REFRESH_TIMEOUT,
            })?
    }

    async fn refresh(
        &mut self,
        runtime: &Mutex<GatewayRuntime>,
        registry: &PingoraRouteRegistry,
    ) -> Result<GatewayRuntimeTick, GatewayProcessRuntimeError> {
        let update = self.load_update().await?;
        let tick = {
            let mut runtime = runtime
                .lock()
                .expect("gateway runtime lock is not poisoned");
            runtime.apply_source_update(update)
        };
        if let Some(projection) = tick.served.as_ref() {
            registry
                .replace_projection(projection)
                .map_err(GatewayProcessRuntimeError::UpdatePingoraRoutes)?;
        }

        Ok(tick)
    }

    async fn load_update(&mut self) -> Result<GatewayProjectionUpdate, GatewayProcessRuntimeError> {
        let stores = self.stores().await?;
        Ok(
            load_gateway_projection_update_from_nats(&stores.core_state, &stores.observations)
                .await,
        )
    }

    async fn replace_gateway_status(
        &mut self,
        status: &GatewayStatusObservation,
    ) -> Result<(), GatewayProcessRuntimeError> {
        let stores = self.stores().await?;
        stores
            .observations
            .replace_gateway_status(status)
            .await
            .map_err(GatewayProcessRuntimeError::WriteObservations)
    }

    async fn stores(&mut self) -> Result<&GatewayProcessStores, GatewayProcessRuntimeError> {
        let client = &self.client;
        self.stores
            .get_or_open(async || open_gateway_process_stores(client.clone()).await)
            .await
    }
}

struct GatewayProcessStores {
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
}

async fn open_gateway_process_stores(
    client: NatsClient,
) -> Result<GatewayProcessStores, GatewayProcessRuntimeError> {
    let jetstream = async_nats::jetstream::new(client);
    let open_core_state = async {
        AsyncNatsCoreStateStore::from_jetstream(&jetstream)
            .await
            .map_err(GatewayProcessRuntimeError::OpenCoreState)
    };
    let open_observations = async {
        AsyncNatsObservationStore::from_jetstream(&jetstream)
            .await
            .map_err(GatewayProcessRuntimeError::OpenObservations)
    };
    let (core_state, observations) = tokio::try_join!(open_core_state, open_observations)?;

    Ok(GatewayProcessStores {
        core_state,
        observations,
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
    routes: async_nats::jetstream::kv::Watch,
    observations: async_nats::jetstream::kv::Watch,
}

async fn open_gateway_change_watchers(
    client: NatsClient,
) -> Result<GatewayChangeWatchers, GatewayProcessRuntimeError> {
    let stores = open_gateway_process_stores(client).await?;
    let routes = stores
        .core_state
        .watch_route_binding_changes()
        .await
        .map_err(GatewayProcessRuntimeError::WatchRoutes)?;
    let observations = stores
        .observations
        .watch_machine_container_snapshot_changes()
        .await
        .map_err(GatewayProcessRuntimeError::WatchObservations)?;

    Ok(GatewayChangeWatchers {
        routes,
        observations,
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
            route = watchers.routes.next() => {
                match watch_event_change("routes", route) {
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
            observation = watchers.observations.next() => {
                match watch_event_change("observations", observation) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayWatchLoopEnd {
    Restart,
    Shutdown,
}

enum GatewayWatchEvent {
    Changed,
    Failed(GatewayWatchFailure),
}

fn watch_event_change(
    source: &'static str,
    event: Option<
        Result<async_nats::jetstream::kv::Entry, async_nats::jetstream::kv::WatcherError>,
    >,
) -> GatewayWatchEvent {
    match event {
        Some(Ok(_)) => GatewayWatchEvent::Changed,
        Some(Err(error)) => GatewayWatchEvent::Failed(GatewayWatchFailure::Stream {
            message: error.to_string(),
        }),
        None => GatewayWatchEvent::Failed(GatewayWatchFailure::Ended { source }),
    }
}

fn record_gateway_watch_success(health: &Mutex<GatewayProcessHealth>) {
    let mut health = health.lock().expect("gateway health lock is not poisoned");
    health.last_watch_failure = None;
    health.consecutive_watch_failures = 0;
}

fn record_gateway_watch_failure(
    health: &Mutex<GatewayProcessHealth>,
    failure: GatewayWatchFailure,
) {
    let mut health = health.lock().expect("gateway health lock is not poisoned");
    health.last_watch_failure = Some(failure);
    health.consecutive_watch_failures += 1;
}

fn record_gateway_status_publish_result(
    health: &Mutex<GatewayProcessHealth>,
    result: Result<(), GatewayProcessRuntimeError>,
) {
    let mut health = health.lock().expect("gateway health lock is not poisoned");
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
    attempt: Result<GatewayRuntimeTick, GatewayProcessRuntimeError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health.lock().expect("gateway health lock is not poisoned");
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

fn gateway_attempt_from_tick(tick: GatewayRuntimeTick) -> GatewayProcessAttempt {
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
    attempt: &Result<GatewayRuntimeTick, GatewayProcessRuntimeError>,
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
    let mut health = health.lock().expect("gateway health lock is not poisoned");
    health.last_http_failure = Some(failure);
    health.consecutive_http_failures += 1;
}

async fn resolve_gateway_listen_addr(
    listen_addr: SocketAddr,
) -> Result<SocketAddr, GatewayProcessRuntimeError> {
    let listener = TcpListener::bind(listen_addr).await.map_err(|source| {
        GatewayProcessRuntimeError::BindHttp {
            addr: listen_addr,
            source,
        }
    })?;
    let resolved = listener
        .local_addr()
        .map_err(GatewayProcessRuntimeError::ReadHttpListenerAddr)?;
    drop(listener);
    Ok(resolved)
}

async fn wait_for_gateway_listener_ready(
    listen_addr: SocketAddr,
    http_task: &JoinHandle<()>,
) -> Result<(), GatewayProcessRuntimeError> {
    let deadline = tokio::time::Instant::now() + GATEWAY_LISTENER_READY_TIMEOUT;
    loop {
        if tokio::time::timeout(GATEWAY_LISTENER_READY_POLL, TcpStream::connect(listen_addr))
            .await
            .is_ok_and(|connected| connected.is_ok())
        {
            return Ok(());
        }
        if http_task.is_finished() || tokio::time::Instant::now() >= deadline {
            return Err(GatewayProcessRuntimeError::HttpListenerNotReady {
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
    let mut conf = ServerConf::default();
    conf.grace_period_seconds = Some(0);
    conf.graceful_shutdown_timeout_seconds = Some(1);

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
) -> Result<(), GatewayProcessRuntimeError> {
    let runtime = start_gateway_process_runtime(config).await?;
    shutdown_signal()
        .await
        .map_err(GatewayProcessRuntimeError::ShutdownSignal)?;
    runtime.shutdown().await;
    Ok(())
}

#[derive(Debug)]
pub enum GatewayProcessRuntimeError {
    AwaitCredentials(AwaitSeedFileError),
    ConnectNats(NatsConnectError),
    BindHttp {
        addr: SocketAddr,
        source: std::io::Error,
    },
    ReadHttpListenerAddr(std::io::Error),
    HttpListenerNotReady {
        addr: SocketAddr,
        timeout: Duration,
    },
    OpenCoreState(CoreStateStoreError),
    OpenObservations(ObservationStoreError),
    WriteObservations(ObservationStoreError),
    UpdatePingoraRoutes(PingoraRouteRegistryError),
    WatchRoutes(ployz_nats::core_state::RouteBindingStoreError),
    WatchObservations(ObservationStoreError),
    RefreshTimedOut {
        timeout: Duration,
    },
    ShutdownSignal(std::io::Error),
}

impl fmt::Display for GatewayProcessRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitCredentials(error) => write!(formatter, "{error}"),
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::BindHttp { addr, source } => {
                write!(
                    formatter,
                    "failed to bind gateway HTTP listener {addr}: {source}"
                )
            }
            Self::ReadHttpListenerAddr(error) => {
                write!(
                    formatter,
                    "failed to read gateway HTTP listener address: {error}"
                )
            }
            Self::HttpListenerNotReady { addr, timeout } => {
                write!(
                    formatter,
                    "gateway HTTP listener {addr} was not ready after {}s",
                    timeout.as_secs()
                )
            }
            Self::OpenCoreState(error) => {
                write!(formatter, "failed to open core state store: {error}")
            }
            Self::OpenObservations(error) => {
                write!(formatter, "failed to open observation store: {error}")
            }
            Self::WriteObservations(error) => {
                write!(formatter, "failed to write gateway observation: {error}")
            }
            Self::UpdatePingoraRoutes(error) => {
                write!(
                    formatter,
                    "failed to update Pingora gateway routes: {error}"
                )
            }
            Self::WatchRoutes(error) => write!(formatter, "failed to watch routes: {error}"),
            Self::WatchObservations(error) => {
                write!(formatter, "failed to watch observations: {error}")
            }
            Self::RefreshTimedOut { timeout } => {
                write!(
                    formatter,
                    "gateway projection refresh timed out after {}s",
                    timeout.as_secs()
                )
            }
            Self::ShutdownSignal(error) => {
                write!(formatter, "failed to wait for shutdown: {error}")
            }
        }
    }
}

impl std::error::Error for GatewayProcessRuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::GatewayProjectionError;
    use crate::gateway_runtime::GatewayServingState;

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
            Ok(GatewayRuntimeTick {
                state: crate::gateway::GatewayProjectionState {
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
            Err(GatewayProcessRuntimeError::RefreshTimedOut {
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
