//! Runtime wiring for the gateway role.

use crate::config::GatewayProcessConfig;
use crate::gateway::{GatewayProjection, GatewayProjectionUpdate};
use crate::gateway_runtime::{GatewayRuntime, GatewayRuntimeTick, GatewayServingState};
use crate::gateway_source::load_gateway_projection_update_from_nats;
use ployz_nats::connect::{NatsConnectError, connect_with_timeout};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::service_runtime::NatsClient;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const GATEWAY_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const GATEWAY_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const GATEWAY_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RunningGatewayProcessRuntime {
    runtime: Arc<Mutex<GatewayRuntime>>,
    health: Arc<Mutex<GatewayProcessHealth>>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningGatewayProcessRuntime {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
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
}

pub async fn start_gateway_process_runtime(
    config: &GatewayProcessConfig,
) -> Result<RunningGatewayProcessRuntime, GatewayProcessRuntimeError> {
    let client = connect_with_timeout(&config.nats_url, GATEWAY_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(GatewayProcessRuntimeError::ConnectNats)?;
    Ok(start_gateway_process_runtime_with_client(
        client,
        GATEWAY_REFRESH_INTERVAL,
    ))
}

#[must_use]
pub fn start_gateway_process_runtime_with_client(
    client: NatsClient,
    refresh_interval: Duration,
) -> RunningGatewayProcessRuntime {
    let runtime = Arc::new(Mutex::new(GatewayRuntime::new()));
    let health = Arc::new(Mutex::new(GatewayProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
    }));
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let task_runtime = Arc::clone(&runtime);
    let task_health = Arc::clone(&health);
    let task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let mut source = GatewayProcessSource::new(client);

        loop {
            let attempt = source.refresh_with_timeout(&task_runtime).await;
            backoff = record_gateway_attempt(&task_health, attempt, refresh_interval, backoff);

            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = &mut shutdown_rx => break,
            }
        }
    });

    RunningGatewayProcessRuntime {
        runtime,
        health,
        shutdown,
        task,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcessHealth {
    pub last_attempt: Option<GatewayProcessAttempt>,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayProcessAttempt {
    Current { route_count: usize },
    ServingLastKnownGood { route_count: usize, message: String },
    Failed { message: String },
}

struct GatewayProcessSource {
    client: NatsClient,
    stores: Option<GatewayProcessStores>,
}

impl GatewayProcessSource {
    fn new(client: NatsClient) -> Self {
        Self {
            client,
            stores: None,
        }
    }

    async fn refresh_with_timeout(
        &mut self,
        runtime: &Mutex<GatewayRuntime>,
    ) -> Result<GatewayRuntimeTick, GatewayProcessRuntimeError> {
        tokio::time::timeout(GATEWAY_REFRESH_TIMEOUT, self.refresh(runtime))
            .await
            .map_err(|_| GatewayProcessRuntimeError::RefreshTimedOut {
                timeout: GATEWAY_REFRESH_TIMEOUT,
            })?
    }

    async fn refresh(
        &mut self,
        runtime: &Mutex<GatewayRuntime>,
    ) -> Result<GatewayRuntimeTick, GatewayProcessRuntimeError> {
        let update = self.load_update().await?;
        let mut runtime = runtime
            .lock()
            .expect("gateway runtime lock is not poisoned");

        Ok(runtime.apply_source_update(update))
    }

    async fn load_update(&mut self) -> Result<GatewayProjectionUpdate, GatewayProcessRuntimeError> {
        let stores = self.stores().await?;
        Ok(
            load_gateway_projection_update_from_nats(&stores.core_state, &stores.observations)
                .await,
        )
    }

    async fn stores(&mut self) -> Result<&GatewayProcessStores, GatewayProcessRuntimeError> {
        if self.stores.is_none() {
            let jetstream = async_nats::jetstream::new(self.client.clone());
            self.stores = Some(GatewayProcessStores {
                core_state: AsyncNatsCoreStateStore::from_jetstream(&jetstream)
                    .await
                    .map_err(GatewayProcessRuntimeError::OpenCoreState)?,
                observations: AsyncNatsObservationStore::from_jetstream(&jetstream)
                    .await
                    .map_err(GatewayProcessRuntimeError::OpenObservations)?,
            });
        }

        Ok(self
            .stores
            .as_ref()
            .expect("gateway source stores are opened before refresh"))
    }
}

struct GatewayProcessStores {
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
}

fn record_gateway_attempt(
    health: &Mutex<GatewayProcessHealth>,
    attempt: Result<GatewayRuntimeTick, GatewayProcessRuntimeError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health.lock().expect("gateway health lock is not poisoned");
    match attempt {
        Ok(tick) => {
            let attempt = gateway_attempt_from_tick(tick);
            let is_current = matches!(attempt, GatewayProcessAttempt::Current { .. });
            health.last_attempt = Some(attempt);
            if is_current {
                health.consecutive_failures = 0;
            } else {
                health.consecutive_failures += 1;
            }
            interval
        }
        Err(error) => {
            health.last_attempt = Some(GatewayProcessAttempt::Failed {
                message: error.to_string(),
            });
            health.consecutive_failures += 1;
            next_gateway_backoff(current_backoff)
        }
    }
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

fn next_gateway_backoff(current_backoff: Duration) -> Duration {
    current_backoff
        .saturating_mul(2)
        .min(Duration::from_secs(30))
}

pub async fn run_gateway_until_shutdown(
    config: &GatewayProcessConfig,
) -> Result<(), GatewayProcessRuntimeError> {
    let runtime = start_gateway_process_runtime(config).await?;
    wait_for_shutdown_signal()
        .await
        .map_err(GatewayProcessRuntimeError::ShutdownSignal)?;
    runtime.shutdown().await;
    Ok(())
}

async fn wait_for_shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

#[derive(Debug)]
pub enum GatewayProcessRuntimeError {
    ConnectNats(NatsConnectError),
    OpenCoreState(CoreStateStoreError),
    OpenObservations(ObservationStoreError),
    RefreshTimedOut { timeout: Duration },
    ShutdownSignal(std::io::Error),
}

impl fmt::Display for GatewayProcessRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::OpenCoreState(error) => {
                write!(formatter, "failed to open core state store: {error}")
            }
            Self::OpenObservations(error) => {
                write!(formatter, "failed to open observation store: {error}")
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
        });
        let interval = Duration::from_secs(1);

        let next = record_gateway_attempt(
            &health,
            Ok(GatewayRuntimeTick {
                state: crate::gateway::GatewayProjectionState::ProjectionFailedUnavailable {
                    error: GatewayProjectionError::SourceUnavailable {
                        message: "not used".to_owned(),
                    },
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
