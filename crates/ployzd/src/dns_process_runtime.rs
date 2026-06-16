//! Runtime wiring for the DNS role.
//!
//! `ployzd dns` is a supervised watcher process: it consumes active route
//! state and gateway observations from NATS, keeps a last-known-good answer
//! table, and exposes typed health. It owns no command surface.

use crate::config::DnsProcessConfig;
use crate::dns::{DnsProjection, DnsRuntime, DnsRuntimeTick, DnsServingState};
use crate::dns_source::load_dns_projection_update_from_nats;
use crate::node_credentials::{AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials};
use crate::process_support::{
    BackoffSchedule, LazyHandle, RecordedAttempt, RefreshDelay, drain_refresh_wakes,
    record_attempt, shutdown_signal, sleep_or_shutdown, wait_for_refresh_delay,
};
use futures_util::StreamExt;
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::service_runtime::NatsClient;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

const DNS_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const DNS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_WATCH_RESTART_DELAY: Duration = Duration::from_secs(1);

pub struct RunningDnsProcessRuntime {
    runtime: Arc<Mutex<DnsRuntime>>,
    health: Arc<Mutex<DnsProcessHealth>>,
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
            .expect("dns health lock is not poisoned")
            .clone()
    }

    /// The last-known-good DNS projection this process serves, if any.
    #[must_use]
    pub fn served_projection(&self) -> Option<DnsProjection> {
        self.runtime
            .lock()
            .expect("dns runtime lock is not poisoned")
            .answers()
            .current()
            .cloned()
    }
}

pub async fn start_dns_process_runtime(
    config: &DnsProcessConfig,
) -> Result<RunningDnsProcessRuntime, DnsProcessRuntimeError> {
    // DNS authenticates as the machine's Node user (no DNS principal in
    // v1) and awaits the seed file like the node and gateway roles do.
    let connect =
        await_role_credentials("dns", &config.nats, &SeedFileRetryPolicy::default_policy())
            .await
            .map_err(DnsProcessRuntimeError::AwaitCredentials)?;
    let client = connect_authenticated(&connect, DNS_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(DnsProcessRuntimeError::ConnectNats)?;
    start_dns_process_runtime_with_client(client, DNS_REFRESH_INTERVAL).await
}

pub async fn start_dns_process_runtime_with_client(
    client: NatsClient,
    refresh_interval: Duration,
) -> Result<RunningDnsProcessRuntime, DnsProcessRuntimeError> {
    let runtime = Arc::new(Mutex::new(DnsRuntime::new()));
    let health = Arc::new(Mutex::new(DnsProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
        last_watch_failure: None,
        consecutive_watch_failures: 0,
    }));
    let (shutdown, _) = broadcast::channel(2);
    let (refresh_wake, refresh_wake_rx) = mpsc::channel(1);
    let task_runtime = Arc::clone(&runtime);
    let task_health = Arc::clone(&health);
    let mut refresh_shutdown = shutdown.subscribe();
    let refresh_client = client.clone();
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let mut source = DnsProcessSource::new(refresh_client);
        let mut refresh_wake_rx = refresh_wake_rx;

        loop {
            drain_refresh_wakes(&mut refresh_wake_rx);
            let attempt = source.refresh_with_timeout(&task_runtime).await;
            backoff = record_dns_attempt(&task_health, attempt, refresh_interval, backoff);

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
        wake_dns_refresh_on_nats_changes(
            watch_client,
            refresh_wake,
            watch_health,
            &mut watch_shutdown,
        )
        .await;
    });

    Ok(RunningDnsProcessRuntime {
        runtime,
        health,
        shutdown,
        tasks: vec![refresh_task, watch_task],
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessHealth {
    pub last_attempt: Option<DnsProcessAttempt>,
    pub consecutive_failures: u64,
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
pub enum DnsWatchFailure {
    Open { message: String },
    Stream { message: String },
    Ended { source: &'static str },
}

struct DnsProcessSource {
    client: NatsClient,
    stores: LazyHandle<DnsProcessStores>,
}

impl DnsProcessSource {
    fn new(client: NatsClient) -> Self {
        Self {
            client,
            stores: LazyHandle::new(),
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
        let stores = self.stores().await?;
        let update =
            load_dns_projection_update_from_nats(&stores.core_state, &stores.observations).await;
        let mut runtime = runtime.lock().expect("dns runtime lock is not poisoned");

        Ok(runtime.apply_source_update(update))
    }

    async fn stores(&mut self) -> Result<&DnsProcessStores, DnsProcessRuntimeError> {
        let client = &self.client;
        self.stores
            .get_or_open(async || open_dns_process_stores(client.clone()).await)
            .await
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
    let open_core_state = async {
        AsyncNatsCoreStateStore::from_jetstream(&jetstream)
            .await
            .map_err(DnsProcessRuntimeError::OpenCoreState)
    };
    let open_observations = async {
        AsyncNatsObservationStore::from_jetstream(&jetstream)
            .await
            .map_err(DnsProcessRuntimeError::OpenObservations)
    };
    let (core_state, observations) = tokio::try_join!(open_core_state, open_observations)?;

    Ok(DnsProcessStores {
        core_state,
        observations,
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
        let event = tokio::select! {
            route = watchers.routes.next() => watch_event_change("routes", route),
            status = watchers.gateway_statuses.next() => {
                watch_event_change("gateway statuses", status)
            }
            public_ip = watchers.public_ips.next() => {
                watch_event_change("node public ips", public_ip)
            }
            _ = shutdown.recv() => return DnsWatchLoopEnd::Shutdown,
        };
        match event {
            DnsWatchEvent::Changed => {
                record_dns_watch_success(health);
                let _ = refresh_wake.try_send(());
            }
            DnsWatchEvent::Failed(failure) => {
                record_dns_watch_failure(health, failure);
                return DnsWatchLoopEnd::Restart;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DnsWatchLoopEnd {
    Restart,
    Shutdown,
}

enum DnsWatchEvent {
    Changed,
    Failed(DnsWatchFailure),
}

fn watch_event_change(
    source: &'static str,
    event: Option<
        Result<async_nats::jetstream::kv::Entry, async_nats::jetstream::kv::WatcherError>,
    >,
) -> DnsWatchEvent {
    match event {
        Some(Ok(_)) => DnsWatchEvent::Changed,
        Some(Err(error)) => DnsWatchEvent::Failed(DnsWatchFailure::Stream {
            message: error.to_string(),
        }),
        None => DnsWatchEvent::Failed(DnsWatchFailure::Ended { source }),
    }
}

fn record_dns_watch_success(health: &Mutex<DnsProcessHealth>) {
    let mut health = health.lock().expect("dns health lock is not poisoned");
    health.last_watch_failure = None;
    health.consecutive_watch_failures = 0;
}

fn record_dns_watch_failure(health: &Mutex<DnsProcessHealth>, failure: DnsWatchFailure) {
    let mut health = health.lock().expect("dns health lock is not poisoned");
    health.last_watch_failure = Some(failure);
    health.consecutive_watch_failures += 1;
}

fn record_dns_attempt(
    health: &Mutex<DnsProcessHealth>,
    attempt: Result<DnsRuntimeTick, DnsProcessRuntimeError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health.lock().expect("dns health lock is not poisoned");
    let DnsProcessHealth {
        last_attempt,
        consecutive_failures,
        ..
    } = &mut *health;
    let recorded = match attempt {
        Ok(tick) => {
            let attempt = dns_attempt_from_tick(tick);
            if matches!(attempt, DnsProcessAttempt::Current { .. }) {
                RecordedAttempt::Healthy(attempt)
            } else {
                // Last-known-good serving counts as a failure streak but
                // keeps the steady refresh interval.
                RecordedAttempt::Degraded(attempt)
            }
        }
        Err(error) => RecordedAttempt::Failed(DnsProcessAttempt::Failed {
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

pub async fn run_dns_until_shutdown(
    config: &DnsProcessConfig,
) -> Result<(), DnsProcessRuntimeError> {
    let runtime = start_dns_process_runtime(config).await?;
    shutdown_signal()
        .await
        .map_err(DnsProcessRuntimeError::ShutdownSignal)?;
    runtime.shutdown().await;
    Ok(())
}

#[derive(Debug)]
pub enum DnsProcessRuntimeError {
    AwaitCredentials(AwaitSeedFileError),
    ConnectNats(NatsConnectError),
    OpenCoreState(CoreStateStoreError),
    OpenObservations(ObservationStoreError),
    WatchRoutes(ployz_nats::core_state::ActiveRouteStoreError),
    WatchObservations(ObservationStoreError),
    RefreshTimedOut { timeout: Duration },
    ShutdownSignal(std::io::Error),
}

impl fmt::Display for DnsProcessRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitCredentials(error) => write!(formatter, "{error}"),
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::OpenCoreState(error) => {
                write!(formatter, "failed to open core state store: {error}")
            }
            Self::OpenObservations(error) => {
                write!(formatter, "failed to open observation store: {error}")
            }
            Self::WatchRoutes(error) => write!(formatter, "failed to watch routes: {error}"),
            Self::WatchObservations(error) => {
                write!(formatter, "failed to watch observations: {error}")
            }
            Self::RefreshTimedOut { timeout } => {
                write!(
                    formatter,
                    "DNS projection refresh timed out after {}s",
                    timeout.as_secs()
                )
            }
            Self::ShutdownSignal(error) => {
                write!(formatter, "failed to wait for shutdown: {error}")
            }
        }
    }
}

impl std::error::Error for DnsProcessRuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::{DnsProjectionError, DnsProjectionState};

    #[test]
    fn retained_last_good_attempt_keeps_steady_refresh_interval() {
        let health = Mutex::new(DnsProcessHealth {
            last_attempt: None,
            consecutive_failures: 0,
            last_watch_failure: None,
            consecutive_watch_failures: 0,
        });
        let interval = Duration::from_secs(1);

        let next = record_dns_attempt(
            &health,
            Ok(DnsRuntimeTick {
                state: DnsProjectionState::Unavailable,
                served: None,
                serving: DnsServingState::LastKnownGood {
                    record_count: 1,
                    error: DnsProjectionError::InvalidSource {
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
                .expect("dns health lock is not poisoned")
                .last_attempt,
            Some(DnsProcessAttempt::ServingLastKnownGood {
                record_count: 1,
                message: "InvalidSource { message: \"nats unavailable\" }".to_owned(),
            })
        );
    }

    #[test]
    fn refresh_runtime_error_uses_exponential_backoff() {
        let health = Mutex::new(DnsProcessHealth {
            last_attempt: None,
            consecutive_failures: 0,
            last_watch_failure: None,
            consecutive_watch_failures: 0,
        });

        let next = record_dns_attempt(
            &health,
            Err(DnsProcessRuntimeError::RefreshTimedOut {
                timeout: Duration::from_secs(5),
            }),
            Duration::from_secs(1),
            Duration::from_secs(2),
        );

        assert_eq!(next, Duration::from_secs(4));
    }
}
