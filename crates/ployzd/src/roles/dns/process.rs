//! Process wiring for the DNS role.
//!
//! `ployzd dns` is a supervised watcher process: it consumes route binding
//! state, gateway observations, and machine facts from NATS, keeps a
//! last-known-good public answer table and machine fact cache, and exposes
//! typed health. It owns no command surface.

use crate::adapters::credentials::{
    AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials,
};
use crate::config::DnsProcessConfig;
use crate::fact_cache::{FactCache, FactCacheError, RunningFactCache, start_fact_cache};
use crate::intent::service::NatsIntentReader;
use crate::process_support::{BackoffSchedule, RecordedAttempt, record_attempt, shutdown_signal};
use crate::roles::dns::InternalResolverHealth;
use crate::roles::dns::internal::{InternalDnsIntentCache, spawn_internal_resolver};
use crate::roles::dns::projection::{
    DnsProjection, DnsProjector, DnsProjectorTick, DnsServingState,
};
use crate::roles::dns::service::start_dns_role_service;
use crate::roles::dns::source::load_dns_source_refresh_from_nats;
use crate::roles::machine::intent_mirror::MachineIntentMirror;
use crate::roles::nats_failover::{
    IntentFailover, mirrored_server_pool, spawn_intent_failover_mirror,
};
use ployz_nats::connect::{NatsConnectError, connect_authenticated_pool};
use ployz_nats::service_runtime::{NatsClient, NatsServiceRuntimeError, RunningNatsService};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const DNS_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const DNS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RunningDnsProcess {
    runtime: Arc<Mutex<DnsProjector>>,
    health: Arc<Mutex<DnsProcessHealth>>,
    internal_resolver_health: Option<Arc<Mutex<InternalResolverHealth>>>,
    shutdown: broadcast::Sender<()>,
    role_service: RunningNatsService,
    facts_cache: RunningFactCache,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningDnsProcess {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        for task in self.tasks {
            let _ = task.await;
        }
        let _ = self.role_service.shutdown().await;
        self.facts_cache.shutdown().await;
    }

    #[must_use]
    pub fn health(&self) -> DnsProcessHealth {
        self.health
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The last-known-good DNS projection this process serves, if any.
    #[must_use]
    pub fn served_projection(&self) -> Option<DnsProjection> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .answers()
            .current()
            .cloned()
    }

    #[must_use]
    pub fn internal_resolver_health(&self) -> Option<InternalResolverHealth> {
        self.internal_resolver_health.as_ref().map(|health| {
            health
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
    }
}

pub async fn start_dns_process(
    config: &DnsProcessConfig,
) -> Result<RunningDnsProcess, DnsProcessError> {
    // DNS authenticates as the machine's Machine user (no DNS principal in
    // v1) and awaits the seed file like the machine and gateway roles do.
    let connect =
        await_role_credentials("dns", &config.nats, &SeedFileRetryPolicy::default_policy())
            .await
            .map_err(DnsProcessError::AwaitCredentials)?;
    let mirror = MachineIntentMirror::beside_seed_file(&config.nats.seed_file);
    let pool = mirrored_server_pool(&mirror, &connect.url);
    let client = connect_authenticated_pool(&connect, &pool, DNS_NATS_CONNECT_TIMEOUT)
        .await
        .map_err(DnsProcessError::ConnectNats)?;
    let internal_resolver_bind =
        SocketAddr::new(IpAddr::V4(config.endpoint_subnet.bridge_gateway_ipv4()), 53);
    start_dns_process_with_client(
        client,
        config.machine_id.clone(),
        DNS_REFRESH_INTERVAL,
        Some(IntentFailover {
            mirror,
            seed: connect.url,
        }),
        Some(internal_resolver_bind),
    )
    .await
}

pub async fn start_dns_process_with_client(
    client: NatsClient,
    machine_id: ployz_core::ids::MachineId,
    refresh_interval: Duration,
    failover: Option<IntentFailover>,
    internal_resolver_bind: Option<SocketAddr>,
) -> Result<RunningDnsProcess, DnsProcessError> {
    let runtime = Arc::new(Mutex::new(DnsProjector::new()));
    let health = Arc::new(Mutex::new(DnsProcessHealth {
        last_attempt: None,
        consecutive_failures: 0,
    }));
    let facts_cache = start_fact_cache(client.clone())
        .await
        .map_err(DnsProcessError::StartFactsCache)?;
    let stores = open_dns_process_stores(client.clone(), facts_cache.cache());
    let internal_dns_intent = InternalDnsIntentCache::default();
    internal_dns_intent.record_if_available(
        failover
            .as_ref()
            .and_then(|failover| failover.mirror.load()),
    );
    let (shutdown, _) = broadcast::channel(2);
    let mut tasks = Vec::new();
    let internal_resolver_health = internal_resolver_bind.map(|bind| {
        let health = Arc::new(Mutex::new(InternalResolverHealth::AwaitingBind {
            attempts: 0,
        }));
        tasks.push(spawn_internal_resolver(
            facts_cache.cache(),
            internal_dns_intent.clone(),
            bind,
            shutdown.subscribe(),
            Arc::clone(&health),
        ));
        health
    });
    let role_service = match start_dns_role_service(
        client.clone(),
        machine_id,
        facts_cache.cache(),
        internal_resolver_health.clone(),
    )
    .await
    {
        Ok(service) => service,
        Err(error) => {
            let _ = shutdown.send(());
            for task in tasks {
                let _ = task.await;
            }
            facts_cache.shutdown().await;
            return Err(DnsProcessError::StartRoleService(error));
        }
    };
    if let Some(failover) = failover {
        tasks.push(spawn_intent_failover_mirror(
            client.clone(),
            failover,
            shutdown.subscribe(),
        ));
    }
    let task_runtime = Arc::clone(&runtime);
    let task_health = Arc::clone(&health);
    let mut refresh_shutdown = shutdown.subscribe();
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let source = DnsProcessSource::new(stores, internal_dns_intent);

        loop {
            let attempt = source.refresh_with_timeout(&task_runtime).await;
            backoff = record_dns_attempt(&task_health, attempt, refresh_interval, backoff);

            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                _ = refresh_shutdown.recv() => break,
            }
        }
    });

    tasks.push(refresh_task);

    Ok(RunningDnsProcess {
        runtime,
        health,
        internal_resolver_health,
        shutdown,
        role_service,
        facts_cache,
        tasks,
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
    internal_dns_intent: InternalDnsIntentCache,
}

impl DnsProcessSource {
    fn new(stores: DnsProcessStores, internal_dns_intent: InternalDnsIntentCache) -> Self {
        Self {
            stores,
            internal_dns_intent,
        }
    }

    async fn refresh_with_timeout(
        &self,
        runtime: &Mutex<DnsProjector>,
    ) -> Result<DnsProjectorTick, DnsProcessError> {
        tokio::time::timeout(DNS_REFRESH_TIMEOUT, self.refresh(runtime))
            .await
            .map_err(|_| DnsProcessError::RefreshTimedOut {
                timeout: DNS_REFRESH_TIMEOUT,
            })?
    }

    async fn refresh(
        &self,
        runtime: &Mutex<DnsProjector>,
    ) -> Result<DnsProjectorTick, DnsProcessError> {
        let refresh =
            load_dns_source_refresh_from_nats(&self.stores.intent_reader, &self.stores.facts).await;
        self.internal_dns_intent.record_if_available(refresh.intent);
        let mut runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        Ok(runtime.apply_source_update(refresh.projection))
    }
}

struct DnsProcessStores {
    intent_reader: NatsIntentReader,
    facts: FactCache,
}

fn open_dns_process_stores(client: NatsClient, facts: FactCache) -> DnsProcessStores {
    DnsProcessStores {
        intent_reader: NatsIntentReader::new(client),
        facts,
    }
}

fn record_dns_attempt(
    health: &Mutex<DnsProcessHealth>,
    attempt: Result<DnsProjectorTick, DnsProcessError>,
    interval: Duration,
    current_backoff: Duration,
) -> Duration {
    let mut health = health
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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

fn dns_attempt_from_tick(tick: DnsProjectorTick) -> DnsProcessAttempt {
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

pub async fn run_dns_until_shutdown(config: &DnsProcessConfig) -> Result<(), DnsProcessError> {
    let runtime = start_dns_process(config).await?;
    shutdown_signal()
        .await
        .map_err(DnsProcessError::ShutdownSignal)?;
    runtime.shutdown().await;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum DnsProcessError {
    #[error("{0}")]
    AwaitCredentials(AwaitSeedFileError),
    #[error("{0}")]
    ConnectNats(NatsConnectError),
    #[error("failed to start runtime facts cache: {0}")]
    StartFactsCache(FactCacheError),
    #[error("failed to start DNS role query service: {0}")]
    StartRoleService(NatsServiceRuntimeError),
    #[error("DNS projection refresh timed out after {}s", timeout.as_secs())]
    RefreshTimedOut { timeout: Duration },
    #[error("failed to wait for shutdown: {0}")]
    ShutdownSignal(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::dns::projection::{DnsProjectionError, DnsProjectionState};

    #[test]
    fn retained_last_good_attempt_keeps_steady_refresh_interval() {
        let health = Mutex::new(DnsProcessHealth {
            last_attempt: None,
            consecutive_failures: 0,
        });
        let interval = Duration::from_secs(1);

        let next = record_dns_attempt(
            &health,
            Ok(DnsProjectorTick {
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
            Err(DnsProcessError::RefreshTimedOut {
                timeout: Duration::from_secs(5),
            }),
            Duration::from_secs(1),
            Duration::from_secs(2),
        );

        assert_eq!(next, Duration::from_secs(4));
    }
}
