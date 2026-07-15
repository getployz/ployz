//! Process wiring for the DNS role.
//!
//! `ployzd dns` is a supervised machine-local resolver: it consumes serving
//! intent and machine facts from NATS, answers internal service names, forwards
//! other queries upstream, and exposes machine-scoped resolve and status RPC.

use crate::adapters::credentials::{
    AwaitSeedFileError, SeedFileRetryPolicy, await_role_credentials,
};
use crate::config::DnsProcessConfig;
use crate::intent::service::NatsIntentReader;
use crate::process_support::{
    RefreshDelay, bounded_role_shutdown, drain_refresh_wakes, shutdown_signal, sleep_or_shutdown,
    wait_for_refresh_delay,
};
use crate::recovery::IntentMirror;
use crate::recovery::{IntentFailover, mirrored_server_pool, spawn_intent_failover_mirror};
use crate::role_testimony::{
    RoleTestimonyCacheError, RunningRoleTestimonyCache, start_role_testimony_cache,
};
use crate::roles::dns::InternalResolverHealth;
use crate::roles::dns::internal::{InternalDnsIntentCache, spawn_internal_resolver};
use crate::roles::dns::service::start_dns_role_service;
use futures_util::StreamExt;
use ployz_core::subjects::INTENT_CHANGED;
use ployz_nats::connect::{NatsConnectError, connect_authenticated_pool};
use ployz_nats::service_runtime::{
    NatsClient, NatsServiceRuntimeError, NatsServiceShutdownError, RunningNatsService,
};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

const DNS_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const DNS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const DNS_WATCH_RESTART_DELAY: Duration = Duration::from_secs(1);
const DNS_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct RunningDnsProcess {
    internal_resolver_health: Option<Arc<Mutex<InternalResolverHealth>>>,
    shutdown: broadcast::Sender<()>,
    role_service: RunningNatsService,
    testimony_cache: RunningRoleTestimonyCache,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningDnsProcess {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        let Self {
            shutdown,
            role_service,
            testimony_cache,
            mut tasks,
            ..
        } = self;
        let _ = shutdown.send(());
        let abort_handles = tasks
            .iter()
            .map(JoinHandle::abort_handle)
            .collect::<Vec<_>>();
        let cleanup = async {
            testimony_cache.shutdown().await;
            let service_shutdown = role_service.shutdown().await;
            for task in &mut tasks {
                let _ = task.await;
            }
            service_shutdown
        };
        bounded_role_shutdown("DNS", DNS_SHUTDOWN_TIMEOUT, &abort_handles, cleanup).await
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
    let mirror = IntentMirror::beside_seed_file(&config.nats.seed_file);
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
    let testimony_cache = start_role_testimony_cache(client.clone())
        .await
        .map_err(DnsProcessError::StartFactsCache)?;
    let intent_reader = NatsIntentReader::new(client.clone());
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
            testimony_cache.cache(),
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
        testimony_cache.cache(),
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
            testimony_cache.shutdown().await;
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
    let (refresh_wake, refresh_wake_rx) = mpsc::channel(1);
    let mut refresh_shutdown = shutdown.subscribe();
    let refresh_task = tokio::spawn(async move {
        let mut backoff = refresh_interval;
        let mut refresh_wake_rx = refresh_wake_rx;

        loop {
            drain_refresh_wakes(&mut refresh_wake_rx);
            match tokio::time::timeout(DNS_REFRESH_TIMEOUT, intent_reader.intent()).await {
                Ok(Ok(intent)) => {
                    internal_dns_intent.record_if_available(Some(intent));
                    backoff = refresh_interval;
                }
                Ok(Err(error)) => {
                    eprintln!("ployzd internal DNS warning: phase=intent-refresh error={error}");
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
                }
                Err(_) => {
                    eprintln!(
                        "ployzd internal DNS warning: phase=intent-refresh error=timed-out timeout_seconds={}",
                        DNS_REFRESH_TIMEOUT.as_secs()
                    );
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
                }
            }

            match wait_for_refresh_delay(backoff, &mut refresh_wake_rx, &mut refresh_shutdown).await
            {
                RefreshDelay::Elapsed | RefreshDelay::Woken => {}
                RefreshDelay::WakeClosed | RefreshDelay::Shutdown => break,
            }
        }
    });
    let mut watch_shutdown = shutdown.subscribe();
    let watch_task = tokio::spawn(async move {
        wake_dns_refresh_on_intent_changed(client, refresh_wake, &mut watch_shutdown).await;
    });

    tasks.push(refresh_task);
    tasks.push(watch_task);

    Ok(RunningDnsProcess {
        internal_resolver_health,
        shutdown,
        role_service,
        testimony_cache,
        tasks,
    })
}

async fn wake_dns_refresh_on_intent_changed(
    client: NatsClient,
    refresh_wake: mpsc::Sender<()>,
    shutdown: &mut broadcast::Receiver<()>,
) {
    loop {
        let opened = tokio::select! {
            opened = client.subscribe(INTENT_CHANGED) => opened,
            _ = shutdown.recv() => break,
        };
        let mut changed = match opened {
            Ok(changed) => changed,
            Err(error) => {
                eprintln!("ployzd internal DNS warning: phase=intent-watch error={error}");
                if sleep_or_shutdown(DNS_WATCH_RESTART_DELAY, shutdown).await {
                    break;
                }
                continue;
            }
        };
        let _ = refresh_wake.try_send(());

        loop {
            tokio::select! {
                change = changed.next() => {
                    if change.is_none() {
                        eprintln!(
                            "ployzd internal DNS warning: phase=intent-watch error=subscription-closed"
                        );
                        break;
                    }
                    let _ = refresh_wake.try_send(());
                }
                _ = shutdown.recv() => return,
            }
        }
    }
}

pub async fn run_dns_until_shutdown(config: &DnsProcessConfig) -> Result<(), DnsProcessError> {
    let shutdown = shutdown_signal().map_err(DnsProcessError::ShutdownSignal)?;
    let runtime = start_dns_process(config).await?;
    shutdown.await.map_err(DnsProcessError::ShutdownSignal)?;
    runtime
        .shutdown()
        .await
        .map_err(DnsProcessError::ShutdownRoleService)
}

#[derive(Debug, thiserror::Error)]
pub enum DnsProcessError {
    #[error("{0}")]
    AwaitCredentials(AwaitSeedFileError),
    #[error("{0}")]
    ConnectNats(NatsConnectError),
    #[error("failed to start role testimony cache: {0}")]
    StartFactsCache(RoleTestimonyCacheError),
    #[error("failed to start DNS role query service: {0}")]
    StartRoleService(NatsServiceRuntimeError),
    #[error("failed to wait for shutdown: {0}")]
    ShutdownSignal(std::io::Error),
    #[error("failed to stop DNS role service: {0:?}")]
    ShutdownRoleService(NatsServiceShutdownError),
}
