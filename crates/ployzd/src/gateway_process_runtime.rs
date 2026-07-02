//! Runtime wiring for the gateway role.

use crate::config::GatewayProcessConfig;
use crate::gateway::{GatewayProjection, GatewayProjectionUpdate};
use crate::gateway_http::{
    GatewayHttpProxyError, proxy_connection_by_first_http_host, write_gateway_http_error_response,
};
use crate::gateway_runtime::{GatewayRuntime, GatewayRuntimeTick, GatewayServingState};
use crate::gateway_source::load_gateway_projection_update_from_nats;
use crate::machine_credentials::{AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials};
use crate::process_support::{BackoffSchedule, RecordedAttempt, record_attempt, shutdown_signal};
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
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const GATEWAY_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const GATEWAY_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const GATEWAY_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RunningGatewayProcessRuntime {
    runtime: Arc<Mutex<GatewayRuntime>>,
    health: Arc<Mutex<GatewayProcessHealth>>,
    listen_addr: SocketAddr,
    shutdown: broadcast::Sender<()>,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningGatewayProcessRuntime {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
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
    let listener = TcpListener::bind(listen_addr).await.map_err(|source| {
        GatewayProcessRuntimeError::BindHttp {
            addr: listen_addr,
            source,
        }
    })?;
    let listen_addr = listener
        .local_addr()
        .map_err(GatewayProcessRuntimeError::ReadHttpListenerAddr)?;
    let listener_port =
        RoutePort::try_new(listen_addr.port()).expect("bound TCP listener port is non-zero");
    let runtime = Arc::new(Mutex::new(GatewayRuntime::new()));
    let health = Arc::new(Mutex::new(GatewayProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
        last_http_failure: None,
        consecutive_http_failures: 0,
        last_status_publish_failure: None,
        consecutive_status_publish_failures: 0,
    }));
    let stores = open_gateway_process_stores(client).await?;
    let (shutdown, _) = broadcast::channel(2);
    let task_runtime = Arc::clone(&runtime);
    let task_health = Arc::clone(&health);
    let mut refresh_shutdown = shutdown.subscribe();
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let source = GatewayProcessSource::new(stores);

        loop {
            let attempt = source.refresh_with_timeout(&task_runtime).await;
            let observed = gateway_observation_from_attempt(&machine_id, listen_addr, &attempt);
            backoff = record_gateway_attempt(&task_health, attempt, refresh_interval, backoff);
            record_gateway_status_publish_result(
                &task_health,
                source.replace_gateway_status(&observed).await,
            );

            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = refresh_shutdown.recv() => break,
            }
        }
    });
    let http_runtime = Arc::clone(&runtime);
    let http_health = Arc::clone(&health);
    let mut http_shutdown = shutdown.subscribe();
    let http_task = tokio::spawn(async move {
        serve_gateway_http(
            listener,
            listener_port,
            http_runtime,
            http_health,
            &mut http_shutdown,
        )
        .await;
    });

    Ok(RunningGatewayProcessRuntime {
        runtime,
        health,
        listen_addr,
        shutdown,
        tasks: vec![refresh_task, http_task],
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessHealth {
    pub last_attempt: Option<GatewayProcessAttempt>,
    pub consecutive_failures: u64,
    pub last_http_failure: Option<GatewayHttpFailure>,
    pub consecutive_http_failures: u64,
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
pub enum GatewayStatusPublishFailure {
    Write { message: String },
}

struct GatewayProcessSource {
    stores: GatewayProcessStores,
}

impl GatewayProcessSource {
    fn new(stores: GatewayProcessStores) -> Self {
        Self { stores }
    }

    async fn refresh_with_timeout(
        &self,
        runtime: &Mutex<GatewayRuntime>,
    ) -> Result<GatewayRuntimeTick, GatewayProcessRuntimeError> {
        tokio::time::timeout(GATEWAY_REFRESH_TIMEOUT, self.refresh(runtime))
            .await
            .map_err(|_| GatewayProcessRuntimeError::RefreshTimedOut {
                timeout: GATEWAY_REFRESH_TIMEOUT,
            })?
    }

    async fn refresh(
        &self,
        runtime: &Mutex<GatewayRuntime>,
    ) -> Result<GatewayRuntimeTick, GatewayProcessRuntimeError> {
        let update = self.load_update().await?;
        let mut runtime = runtime
            .lock()
            .expect("gateway runtime lock is not poisoned");

        Ok(runtime.apply_source_update(update))
    }

    async fn load_update(&self) -> Result<GatewayProjectionUpdate, GatewayProcessRuntimeError> {
        Ok(load_gateway_projection_update_from_nats(
            &self.stores.core_state,
            &self.stores.observations,
        )
        .await)
    }

    async fn replace_gateway_status(
        &self,
        status: &GatewayStatusObservation,
    ) -> Result<(), GatewayProcessRuntimeError> {
        self.stores
            .observations
            .replace_gateway_status(status)
            .await
            .map_err(GatewayProcessRuntimeError::WriteObservations)
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

async fn serve_gateway_http(
    listener: TcpListener,
    listener_port: RoutePort,
    runtime: Arc<Mutex<GatewayRuntime>>,
    health: Arc<Mutex<GatewayProcessHealth>>,
    shutdown: &mut broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        proxy_one_gateway_http_connection(stream, listener_port, &runtime, &health, shutdown).await;
                    }
                    Err(error) => {
                        record_gateway_http_failure(&health, GatewayHttpFailure::Accept {
                            message: error.to_string(),
                        });
                    }
                }
            }
            _ = shutdown.recv() => break,
        }
    }
}

async fn proxy_one_gateway_http_connection(
    mut stream: TcpStream,
    listener_port: RoutePort,
    runtime: &Arc<Mutex<GatewayRuntime>>,
    health: &Arc<Mutex<GatewayProcessHealth>>,
    shutdown: &mut broadcast::Receiver<()>,
) {
    let routes = runtime
        .lock()
        .expect("gateway runtime lock is not poisoned")
        .route_table()
        .clone();

    tokio::select! {
        result = proxy_gateway_http_connection(routes, listener_port, &mut stream) => {
            record_gateway_http_result(health, result);
        }
        _ = shutdown.recv() => {}
    }
}

async fn proxy_gateway_http_connection(
    routes: crate::gateway_runtime::GatewayRouteTable,
    listener_port: RoutePort,
    stream: &mut TcpStream,
) -> Result<(), GatewayHttpProxyError> {
    let result = proxy_connection_by_first_http_host(&routes, stream, listener_port).await;
    if let Err(error) = &result {
        let _ = write_gateway_http_error_response(stream, error).await;
    }

    result
}

fn record_gateway_http_result(
    health: &Mutex<GatewayProcessHealth>,
    result: Result<(), GatewayHttpProxyError>,
) {
    match result {
        Ok(()) => {
            let mut health = health.lock().expect("gateway health lock is not poisoned");
            health.consecutive_http_failures = 0;
        }
        Err(error) => {
            record_gateway_http_failure(
                health,
                GatewayHttpFailure::Proxy {
                    message: error.to_string(),
                },
            );
        }
    }
}

fn record_gateway_http_failure(health: &Mutex<GatewayProcessHealth>, failure: GatewayHttpFailure) {
    let mut health = health.lock().expect("gateway health lock is not poisoned");
    health.last_http_failure = Some(failure);
    health.consecutive_http_failures += 1;
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
    OpenCoreState(CoreStateStoreError),
    OpenObservations(ObservationStoreError),
    WriteObservations(ObservationStoreError),
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
            Self::OpenCoreState(error) => {
                write!(formatter, "failed to open core state store: {error}")
            }
            Self::OpenObservations(error) => {
                write!(formatter, "failed to open observation store: {error}")
            }
            Self::WriteObservations(error) => {
                write!(formatter, "failed to write gateway observation: {error}")
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
}
