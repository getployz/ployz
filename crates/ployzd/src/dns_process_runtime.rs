//! Runtime wiring for the DNS role.
//!
//! `ployzd dns` is a supervised watcher process: it consumes route binding
//! state and gateway observations from NATS, keeps a last-known-good answer
//! table, and exposes typed health. It owns no command surface.

use crate::config::DnsProcessConfig;
use crate::dns::{DnsProjection, DnsRuntime, DnsRuntimeTick, DnsServingState};
use crate::dns_source::load_dns_projection_update_from_nats;
use crate::intent::NatsIntentReader;
use crate::machine_credentials::{AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials};
use crate::process_support::{BackoffSchedule, RecordedAttempt, record_attempt, shutdown_signal};
use crate::fact_cache::{
    RunningRuntimeFactsCache, RuntimeFactsCache, RuntimeFactsCacheError, start_runtime_facts_cache,
};
use ployz_nats::connect::{NatsConnectError, connect_authenticated};
use ployz_nats::service_runtime::NatsClient;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const DNS_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const DNS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RunningDnsProcessRuntime {
    runtime: Arc<Mutex<DnsRuntime>>,
    health: Arc<Mutex<DnsProcessHealth>>,
    shutdown: broadcast::Sender<()>,
    facts_cache: RunningRuntimeFactsCache,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningDnsProcessRuntime {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        for task in self.tasks {
            let _ = task.await;
        }
        self.facts_cache.shutdown().await;
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
    // DNS authenticates as the machine's Machine user (no DNS principal in
    // v1) and awaits the seed file like the machine and gateway roles do.
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
    }));
    let facts_cache = start_runtime_facts_cache(client.clone())
        .await
        .map_err(DnsProcessRuntimeError::StartFactsCache)?;
    let stores = open_dns_process_stores(client, facts_cache.cache());
    let (shutdown, _) = broadcast::channel(2);
    let task_runtime = Arc::clone(&runtime);
    let task_health = Arc::clone(&health);
    let mut refresh_shutdown = shutdown.subscribe();
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let source = DnsProcessSource::new(stores);

        loop {
            let attempt = source.refresh_with_timeout(&task_runtime).await;
            backoff = record_dns_attempt(&task_health, attempt, refresh_interval, backoff);

            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = refresh_shutdown.recv() => break,
            }
        }
    });

    Ok(RunningDnsProcessRuntime {
        runtime,
        health,
        shutdown,
        facts_cache,
        tasks: vec![refresh_task],
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProcessHealth {
    pub last_attempt: Option<DnsProcessAttempt>,
    pub consecutive_failures: u64,
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

struct DnsProcessSource {
    stores: DnsProcessStores,
}

impl DnsProcessSource {
    fn new(stores: DnsProcessStores) -> Self {
        Self { stores }
    }

    async fn refresh_with_timeout(
        &self,
        runtime: &Mutex<DnsRuntime>,
    ) -> Result<DnsRuntimeTick, DnsProcessRuntimeError> {
        tokio::time::timeout(DNS_REFRESH_TIMEOUT, self.refresh(runtime))
            .await
            .map_err(|_| DnsProcessRuntimeError::RefreshTimedOut {
                timeout: DNS_REFRESH_TIMEOUT,
            })?
    }

    async fn refresh(
        &self,
        runtime: &Mutex<DnsRuntime>,
    ) -> Result<DnsRuntimeTick, DnsProcessRuntimeError> {
        let update =
            load_dns_projection_update_from_nats(&self.stores.intent_reader, &self.stores.facts)
                .await;
        let mut runtime = runtime.lock().expect("dns runtime lock is not poisoned");

        Ok(runtime.apply_source_update(update))
    }
}

struct DnsProcessStores {
    intent_reader: NatsIntentReader,
    facts: RuntimeFactsCache,
}

fn open_dns_process_stores(client: NatsClient, facts: RuntimeFactsCache) -> DnsProcessStores {
    DnsProcessStores {
        intent_reader: NatsIntentReader::new(client),
        facts,
    }
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
    StartFactsCache(RuntimeFactsCacheError),
    RefreshTimedOut { timeout: Duration },
    ShutdownSignal(std::io::Error),
}

impl fmt::Display for DnsProcessRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AwaitCredentials(error) => write!(formatter, "{error}"),
            Self::ConnectNats(error) => write!(formatter, "{error}"),
            Self::StartFactsCache(error) => {
                write!(formatter, "failed to start runtime facts cache: {error}")
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
        });
        let interval = Duration::from_secs(1);

        let next = record_dns_attempt(
            &health,
            Ok(DnsRuntimeTick {
                state: DnsProjectionState::unavailable(),
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
