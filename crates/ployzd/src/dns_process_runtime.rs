//! Runtime wiring for the DNS role.

use crate::config::DnsProcessConfig;
use crate::dns::{DnsProjection, DnsProjectionUpdate, DnsRuntime, DnsRuntimeTick, DnsServingState};
use crate::dns_source::load_dns_projection_update_from_nats;
use crate::dns_udp::dns_response_from_query;
use futures_util::StreamExt;
use ployz_nats::connect::{NatsConnectError, connect_with_timeout};
use ployz_nats::core_state::{ActiveRouteReadError, AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::service_runtime::NatsClient;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

const DNS_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const DNS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_WATCH_RESTART_DELAY: Duration = Duration::from_secs(1);

pub struct RunningDnsProcessRuntime {
    runtime: Arc<Mutex<DnsRuntime>>,
    health: Arc<Mutex<DnsProcessHealth>>,
    listen_addr: SocketAddr,
    shutdown: broadcast::Sender<()>,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningDnsProcessRuntime {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        for task in self.tasks {
            let _ = task.await;
        }
    }

    #[must_use]
    pub fn health(&self) -> DnsProcessHealth {
        self.health
            .lock()
            .expect("DNS health lock is not poisoned")
            .clone()
    }

    #[must_use]
    pub fn served_projection(&self) -> Option<DnsProjection> {
        self.runtime
            .lock()
            .expect("DNS runtime lock is not poisoned")
            .answers()
            .current()
            .cloned()
    }

    #[must_use]
    pub const fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }
}

pub async fn start_dns_process_runtime(
    config: &DnsProcessConfig,
) -> Result<RunningDnsProcessRuntime, DnsProcessRuntimeError> {
    let client = connect_with_timeout(&config.nats_url, DNS_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(DnsProcessRuntimeError::ConnectNats)?;
    start_dns_process_runtime_with_client(client, DNS_REFRESH_INTERVAL, config.listen_addr).await
}

pub async fn run_dns_until_shutdown(
    config: &DnsProcessConfig,
) -> Result<(), DnsProcessRuntimeError> {
    let runtime = start_dns_process_runtime(config).await?;
    tokio::signal::ctrl_c()
        .await
        .map_err(DnsProcessRuntimeError::WaitForShutdownSignal)?;
    runtime.shutdown().await;
    Ok(())
}

pub async fn start_dns_process_runtime_with_client(
    client: NatsClient,
    refresh_interval: Duration,
    listen_addr: SocketAddr,
) -> Result<RunningDnsProcessRuntime, DnsProcessRuntimeError> {
    let socket =
        UdpSocket::bind(listen_addr)
            .await
            .map_err(|source| DnsProcessRuntimeError::BindUdp {
                addr: listen_addr,
                source,
            })?;
    let listen_addr = socket
        .local_addr()
        .map_err(DnsProcessRuntimeError::ReadUdpSocketAddr)?;
    let runtime = Arc::new(Mutex::new(DnsRuntime::new()));
    let health = Arc::new(Mutex::new(DnsProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
        last_udp_failure: None,
        consecutive_udp_failures: 0,
        last_watch_failure: None,
        consecutive_watch_failures: 0,
    }));
    let (shutdown, _) = broadcast::channel(2);
    let (refresh_wake, refresh_wake_rx) = mpsc::channel(1);
    let task_runtime = Arc::clone(&runtime);
    let task_health = Arc::clone(&health);
    let refresh_client = client.clone();
    let mut refresh_shutdown = shutdown.subscribe();
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let mut source = DnsProcessSource::new(refresh_client);
        let mut refresh_wake_rx = refresh_wake_rx;

        loop {
            while refresh_wake_rx.try_recv().is_ok() {}
            let attempt = source.refresh_with_timeout(&task_runtime).await;
            backoff = record_dns_attempt(&task_health, attempt, refresh_interval, backoff);

            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                wake = refresh_wake_rx.recv() => {
                    if wake.is_none() {
                        break;
                    }
                }
                _ = refresh_shutdown.recv() => break,
            }
        }
    });
    let watch_client = client.clone();
    let watch_health = Arc::clone(&health);
    let mut watch_shutdown = shutdown.subscribe();
    let watch_task = tokio::spawn(async move {
        wake_dns_refresh_on_nats_changes(
            watch_client,
            refresh_wake,
            watch_health,
            &mut watch_shutdown,
        )
        .await;
    });
    let udp_runtime = Arc::clone(&runtime);
    let udp_health = Arc::clone(&health);
    let mut udp_shutdown = shutdown.subscribe();
    let udp_task = tokio::spawn(async move {
        serve_dns_udp(socket, udp_runtime, udp_health, &mut udp_shutdown).await;
    });

    Ok(RunningDnsProcessRuntime {
        runtime,
        health,
        listen_addr,
        shutdown,
        tasks: vec![refresh_task, watch_task, udp_task],
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessHealth {
    pub last_attempt: Option<DnsProcessAttempt>,
    pub consecutive_failures: u64,
    pub last_udp_failure: Option<DnsUdpFailure>,
    pub consecutive_udp_failures: u64,
    pub last_watch_failure: Option<DnsWatchFailure>,
    pub consecutive_watch_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsProcessAttempt {
    Current {
        record_count: usize,
    },
    ServingLastKnownGood {
        record_count: usize,
        message: String,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsUdpFailure {
    Receive { message: String },
    Send { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsWatchFailure {
    Open { message: String },
    Stream { message: String },
    Ended { source: &'static str },
}

struct DnsProcessSource {
    client: NatsClient,
    stores: Option<DnsProcessStores>,
}

impl DnsProcessSource {
    fn new(client: NatsClient) -> Self {
        Self {
            client,
            stores: None,
        }
    }

    async fn refresh_with_timeout(
        &mut self,
        runtime: &Mutex<DnsRuntime>,
    ) -> Result<DnsRuntimeTick, DnsProcessRuntimeError> {
        tokio::time::timeout(DNS_REFRESH_TIMEOUT, self.refresh(runtime))
            .await
            .map_err(|_| DnsProcessRuntimeError::RefreshTimedOut {
                timeout: DNS_REFRESH_TIMEOUT,
            })?
    }

    async fn refresh(
        &mut self,
        runtime: &Mutex<DnsRuntime>,
    ) -> Result<DnsRuntimeTick, DnsProcessRuntimeError> {
        let update = self.load_update().await?;
        let mut runtime = runtime.lock().expect("DNS runtime lock is not poisoned");
        Ok(runtime.apply_source_update(update))
    }

    async fn load_update(&mut self) -> Result<DnsProjectionUpdate, DnsProcessRuntimeError> {
        let stores = self.stores().await?;
        Ok(load_dns_projection_update_from_nats(&stores.core_state, &stores.observations).await)
    }

    async fn stores(&mut self) -> Result<&DnsProcessStores, DnsProcessRuntimeError> {
        if self.stores.is_none() {
            self.stores = Some(open_dns_process_stores(self.client.clone()).await?);
        }
        Ok(self
            .stores
            .as_ref()
            .expect("DNS stores are opened before use"))
    }
}

struct DnsProcessStores {
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
}

async fn open_dns_process_stores(
    client: NatsClient,
) -> Result<DnsProcessStores, DnsProcessRuntimeError> {
    let jetstream = async_nats::jetstream::new(client);
    Ok(DnsProcessStores {
        core_state: AsyncNatsCoreStateStore::from_jetstream(&jetstream)
            .await
            .map_err(DnsProcessRuntimeError::OpenCoreState)?,
        observations: AsyncNatsObservationStore::from_jetstream(&jetstream)
            .await
            .map_err(DnsProcessRuntimeError::OpenObservations)?,
    })
}

async fn wake_dns_refresh_on_nats_changes(
    client: NatsClient,
    refresh_wake: mpsc::Sender<()>,
    health: Arc<Mutex<DnsProcessHealth>>,
    shutdown: &mut broadcast::Receiver<()>,
) {
    loop {
        let opened = tokio::select! {
            opened = open_dns_change_watchers(client.clone()) => opened,
            _ = shutdown.recv() => break,
        };
        match opened {
            Ok(mut watchers) => {
                match watch_dns_changes(&mut watchers, &refresh_wake, &health, shutdown).await {
                    DnsWatchLoopEnd::Shutdown => break,
                    DnsWatchLoopEnd::Restart => {}
                }
            }
            Err(error) => {
                record_dns_watch_failure(
                    &health,
                    DnsWatchFailure::Open {
                        message: error.to_string(),
                    },
                );
                if sleep_or_shutdown(DNS_WATCH_RESTART_DELAY, shutdown).await {
                    break;
                }
            }
        }
    }
}

struct DnsChangeWatchers {
    routes: async_nats::jetstream::kv::Watch,
    gateway_statuses: async_nats::jetstream::kv::Watch,
    public_ips: async_nats::jetstream::kv::Watch,
}

async fn open_dns_change_watchers(
    client: NatsClient,
) -> Result<DnsChangeWatchers, DnsProcessRuntimeError> {
    let stores = open_dns_process_stores(client).await?;
    let routes = stores
        .core_state
        .watch_active_route_changes()
        .await
        .map_err(DnsProcessRuntimeError::WatchRoutes)?;
    let gateway_statuses = stores
        .observations
        .watch_gateway_status_changes()
        .await
        .map_err(DnsProcessRuntimeError::WatchObservations)?;
    let public_ips = stores
        .observations
        .watch_node_public_ip_changes()
        .await
        .map_err(DnsProcessRuntimeError::WatchObservations)?;

    Ok(DnsChangeWatchers {
        routes,
        gateway_statuses,
        public_ips,
    })
}

async fn watch_dns_changes(
    watchers: &mut DnsChangeWatchers,
    refresh_wake: &mpsc::Sender<()>,
    health: &Mutex<DnsProcessHealth>,
    shutdown: &mut broadcast::Receiver<()>,
) -> DnsWatchLoopEnd {
    loop {
        tokio::select! {
            route = watchers.routes.next() => {
                if handle_dns_watch_event("routes", route, refresh_wake, health) {
                    return DnsWatchLoopEnd::Restart;
                }
            }
            status = watchers.gateway_statuses.next() => {
                if handle_dns_watch_event("gateway_statuses", status, refresh_wake, health) {
                    return DnsWatchLoopEnd::Restart;
                }
            }
            public_ip = watchers.public_ips.next() => {
                if handle_dns_watch_event("public_ips", public_ip, refresh_wake, health) {
                    return DnsWatchLoopEnd::Restart;
                }
            }
            _ = shutdown.recv() => return DnsWatchLoopEnd::Shutdown,
        }
    }
}

fn handle_dns_watch_event(
    source: &'static str,
    event: Option<
        Result<async_nats::jetstream::kv::Entry, async_nats::jetstream::kv::WatcherError>,
    >,
    refresh_wake: &mpsc::Sender<()>,
    health: &Mutex<DnsProcessHealth>,
) -> bool {
    match event {
        Some(Ok(_)) => {
            record_dns_watch_success(health);
            let _ = refresh_wake.try_send(());
            false
        }
        Some(Err(error)) => {
            record_dns_watch_failure(
                health,
                DnsWatchFailure::Stream {
                    message: error.to_string(),
                },
            );
            true
        }
        None => {
            record_dns_watch_failure(health, DnsWatchFailure::Ended { source });
            true
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DnsWatchLoopEnd {
    Restart,
    Shutdown,
}

async fn serve_dns_udp(
    socket: UdpSocket,
    runtime: Arc<Mutex<DnsRuntime>>,
    health: Arc<Mutex<DnsProcessHealth>>,
    shutdown: &mut broadcast::Receiver<()>,
) {
    let mut packet = [0_u8; 512];
    loop {
        tokio::select! {
            received = socket.recv_from(&mut packet) => {
                let (len, peer) = match received {
                    Ok(received) => received,
                    Err(error) => {
                        record_dns_udp_failure(&health, DnsUdpFailure::Receive { message: error.to_string() });
                        continue;
                    }
                };
                let Some(query) = packet.get(..len) else {
                    record_dns_udp_failure(
                        &health,
                        DnsUdpFailure::Receive {
                            message: "received DNS packet length exceeded receive buffer".to_owned(),
                        },
                    );
                    continue;
                };
                let response = {
                    let runtime = runtime.lock().expect("DNS runtime lock is not poisoned");
                    dns_response_from_query(runtime.answers(), query)
                };
                if let Some(response) = response {
                    if let Err(error) = socket.send_to(&response, peer).await {
                        record_dns_udp_failure(&health, DnsUdpFailure::Send { message: error.to_string() });
                    } else {
                        record_dns_udp_success(&health);
                    }
                }
            }
            _ = shutdown.recv() => break,
        }
    }
}

fn record_dns_attempt(
    health: &Mutex<DnsProcessHealth>,
    attempt: Result<DnsRuntimeTick, DnsProcessRuntimeError>,
    refresh_interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health.lock().expect("DNS health lock is not poisoned");
    match attempt {
        Ok(tick) => {
            let attempt = dns_attempt_from_tick(tick);
            let is_current = matches!(attempt, DnsProcessAttempt::Current { .. });
            health.last_attempt = Some(attempt);
            if is_current {
                health.consecutive_failures = 0;
            } else {
                health.consecutive_failures = health.consecutive_failures.saturating_add(1);
            }
            refresh_interval
        }
        Err(error) => {
            health.last_attempt = Some(DnsProcessAttempt::Failed {
                message: error.to_string(),
            });
            health.consecutive_failures = health.consecutive_failures.saturating_add(1);
            next_dns_backoff(current_backoff)
        }
    }
}

fn dns_attempt_from_tick(tick: DnsRuntimeTick) -> DnsProcessAttempt {
    match tick.serving {
        DnsServingState::Current { record_count } => DnsProcessAttempt::Current { record_count },
        DnsServingState::LastKnownGood {
            record_count,
            error,
        } => DnsProcessAttempt::ServingLastKnownGood {
            record_count,
            message: format!("{error:?}"),
        },
        DnsServingState::Unavailable { error } => DnsProcessAttempt::Failed {
            message: error
                .map(|error| format!("{error:?}"))
                .unwrap_or_else(|| "DNS source unavailable".to_owned()),
        },
    }
}

fn next_dns_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(Duration::from_secs(30))
}

fn record_dns_udp_success(health: &Mutex<DnsProcessHealth>) {
    let mut health = health.lock().expect("DNS health lock is not poisoned");
    health.last_udp_failure = None;
    health.consecutive_udp_failures = 0;
}

fn record_dns_udp_failure(health: &Mutex<DnsProcessHealth>, failure: DnsUdpFailure) {
    let mut health = health.lock().expect("DNS health lock is not poisoned");
    health.last_udp_failure = Some(failure);
    health.consecutive_udp_failures = health.consecutive_udp_failures.saturating_add(1);
}

fn record_dns_watch_success(health: &Mutex<DnsProcessHealth>) {
    let mut health = health.lock().expect("DNS health lock is not poisoned");
    health.last_watch_failure = None;
    health.consecutive_watch_failures = 0;
}

fn record_dns_watch_failure(health: &Mutex<DnsProcessHealth>, failure: DnsWatchFailure) {
    let mut health = health.lock().expect("DNS health lock is not poisoned");
    health.last_watch_failure = Some(failure);
    health.consecutive_watch_failures = health.consecutive_watch_failures.saturating_add(1);
}

async fn sleep_or_shutdown(duration: Duration, shutdown: &mut broadcast::Receiver<()>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        _ = shutdown.recv() => true,
    }
}

#[derive(Debug)]
pub enum DnsProcessRuntimeError {
    ConnectNats(NatsConnectError),
    BindUdp {
        addr: SocketAddr,
        source: std::io::Error,
    },
    ReadUdpSocketAddr(std::io::Error),
    OpenCoreState(CoreStateStoreError),
    OpenObservations(ObservationStoreError),
    WatchRoutes(ActiveRouteReadError),
    WatchObservations(ObservationStoreError),
    RefreshTimedOut {
        timeout: Duration,
    },
    WaitForShutdownSignal(std::io::Error),
}

impl fmt::Display for DnsProcessRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::BindUdp { addr, source } => {
                write!(
                    formatter,
                    "failed to bind DNS UDP listener at {addr}: {source}"
                )
            }
            Self::ReadUdpSocketAddr(error) => {
                write!(
                    formatter,
                    "failed to read DNS UDP listener address: {error}"
                )
            }
            Self::OpenCoreState(error) => write!(formatter, "{error}"),
            Self::OpenObservations(error) => write!(formatter, "{error}"),
            Self::WatchRoutes(error) => write!(formatter, "failed to watch active routes: {error}"),
            Self::WatchObservations(error) => {
                write!(formatter, "failed to watch DNS observations: {error}")
            }
            Self::RefreshTimedOut { timeout } => {
                write!(formatter, "DNS refresh timed out after {timeout:?}")
            }
            Self::WaitForShutdownSignal(error) => {
                write!(formatter, "failed to wait for shutdown signal: {error}")
            }
        }
    }
}

impl std::error::Error for DnsProcessRuntimeError {}

#[cfg(test)]
mod tests {
    use super::{DnsProcessAttempt, record_dns_attempt, socketless_health};
    use crate::dns::{
        DnsProjection, DnsProjectionError, DnsProjectionState, DnsRuntimeTick, DnsServingState,
    };
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn retained_last_good_attempt_counts_as_source_failure() {
        let health = Mutex::new(socketless_health());
        let tick = DnsRuntimeTick {
            state: DnsProjectionState::LastKnownGood(DnsProjection {
                records: Vec::new(),
            }),
            served: Some(DnsProjection {
                records: Vec::new(),
            }),
            serving: DnsServingState::LastKnownGood {
                record_count: 0,
                error: DnsProjectionError::InvalidSource {
                    message: "source unavailable".to_owned(),
                },
            },
        };

        record_dns_attempt(
            &health,
            Ok(tick),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );

        let health = health.lock().expect("health lock is not poisoned");
        assert_eq!(
            health.last_attempt,
            Some(DnsProcessAttempt::ServingLastKnownGood {
                record_count: 0,
                message: "InvalidSource { message: \"source unavailable\" }".to_owned(),
            })
        );
        assert_eq!(health.consecutive_failures, 1);
    }
}

#[cfg(test)]
fn socketless_health() -> DnsProcessHealth {
    DnsProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
        last_udp_failure: None,
        consecutive_udp_failures: 0,
        last_watch_failure: None,
        consecutive_watch_failures: 0,
    }
}
