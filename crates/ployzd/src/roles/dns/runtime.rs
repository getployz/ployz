//! DNS role lifecycle: Corrosion projection, resolver replacement, and shutdown.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use ployz_core::network::internal_dns::{
    INTERNAL_DNS_READINESS_ADDRESS, INTERNAL_DNS_READINESS_NAME, InternalDnsRowProjection,
    InternalDnsRowProjectionInput, InternalServiceName, project_internal_dns_rows,
};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::corrosion::CorrosionClient;

use super::config::{DnsRoleConfig, DnsRoleConfigError};
use super::internal::{
    InternalDnsRecords, InternalResolverHealth, query_bound_resolver, spawn_internal_resolver,
};
use super::source::{CorrosionDnsSource, DnsRows, DnsSourceError};

const REFRESH_RETRY_INITIAL: Duration = Duration::from_millis(250);
const REFRESH_RETRY_CAP: Duration = Duration::from_secs(5);
const MAX_CONSECUTIVE_FAILURES: u8 = 8;
const INVALIDATION_COALESCE: Duration = Duration::from_millis(50);
const READINESS_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn run_from_environment() -> Result<(), DnsRoleRuntimeError> {
    let config = DnsRoleConfig::from_environment().map_err(DnsRoleRuntimeError::Configuration)?;
    let client = CorrosionClient::new(config.corrosion().clone())
        .map_err(DnsRoleRuntimeError::CorrosionConfiguration)?;
    let source = CorrosionDnsSource::new(client, config.cluster_id().clone());
    let (shutdown_tx, shutdown) = watch::channel(false);
    let signal = tokio::spawn(async move {
        wait_for_process_shutdown().await;
        let _ = shutdown_tx.send(true);
    });
    let result = run_dns(config, source, shutdown).await;
    signal.abort();
    let _ = signal.await;
    result
}

async fn run_dns(
    config: DnsRoleConfig,
    source: CorrosionDnsSource,
    shutdown: watch::Receiver<bool>,
) -> Result<(), DnsRoleRuntimeError> {
    let mut resolver = ResolverRuntime::default();
    let result = run_dns_loop(config, source, shutdown, &mut resolver).await;
    resolver.stop().await;
    result
}

async fn run_dns_loop(
    config: DnsRoleConfig,
    source: CorrosionDnsSource,
    mut shutdown: watch::Receiver<bool>,
    resolver: &mut ResolverRuntime,
) -> Result<(), DnsRoleRuntimeError> {
    let mut subscriptions = None;
    let mut refresh_retry = RefreshRetry::awaiting_first_projection();

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        if subscriptions.is_none() {
            match until_shutdown(&mut shutdown, source.subscribe()).await {
                ShutdownOutcome::Stopped => {
                    return Ok(());
                }
                ShutdownOutcome::Completed(Ok(active)) => subscriptions = Some(active),
                ShutdownOutcome::Completed(Err(error)) => {
                    retry_failure(&mut refresh_retry, &mut shutdown, error).await?;
                    continue;
                }
            }
        }

        let refresh = until_shutdown(&mut shutdown, source.query_rows()).await;
        let rows = match refresh {
            ShutdownOutcome::Stopped => {
                return Ok(());
            }
            ShutdownOutcome::Completed(Ok(rows)) => rows,
            ShutdownOutcome::Completed(Err(error)) => {
                retry_failure(&mut refresh_retry, &mut shutdown, error).await?;
                continue;
            }
        };
        let projection = match project_rows(&config, rows) {
            Ok(projection) => projection,
            Err(error) => {
                retry_failure(
                    &mut refresh_retry,
                    &mut shutdown,
                    DnsRefreshError::Projection(error.to_string()),
                )
                .await?;
                continue;
            }
        };
        if let Err(error) = resolver.apply(projection).await {
            retry_failure(
                &mut refresh_retry,
                &mut shutdown,
                DnsRefreshError::Resolver(error),
            )
            .await?;
            continue;
        }
        refresh_retry.record_success();

        let Some(active) = subscriptions.as_mut() else {
            continue;
        };
        match until_shutdown(&mut shutdown, active.wait_for_invalidation()).await {
            ShutdownOutcome::Stopped => {
                return Ok(());
            }
            ShutdownOutcome::Completed(Ok(())) => {
                if sleep_or_shutdown(&mut shutdown, INVALIDATION_COALESCE).await {
                    return Ok(());
                }
                if let Err(error) = active.drain_ready_invalidations().await {
                    subscriptions = None;
                    retry_failure(&mut refresh_retry, &mut shutdown, error).await?;
                }
            }
            ShutdownOutcome::Completed(Err(error)) => {
                subscriptions = None;
                retry_failure(&mut refresh_retry, &mut shutdown, error).await?;
            }
        }
    }
}

/// The only daemon-local coupling to Core's row projection contract.
fn project_rows(
    config: &DnsRoleConfig,
    rows: DnsRows,
) -> Result<
    InternalDnsRowProjection,
    ployz_core::network::internal_dns::InternalDnsRowProjectionError,
> {
    project_internal_dns_rows(InternalDnsRowProjectionInput {
        cluster_id: config.cluster_id().clone(),
        local_machine_id: config.local_machine_id().clone(),
        cluster_rows: rows.cluster,
        machine_rows: rows.machines,
        namespace_rows: rows.namespaces,
        service_rows: rows.services,
        container_rows: rows.containers,
    })
}

#[derive(Default)]
struct ResolverRuntime {
    running: Option<RunningResolver>,
}

impl ResolverRuntime {
    async fn apply(&mut self, projection: InternalDnsRowProjection) -> Result<(), String> {
        if let Some(running) = &self.running
            && running.bind == projection.bind
        {
            running
                .records
                .replace(records_with_readiness(projection.records)?);
            return Ok(());
        }

        let replacement = RunningResolver::start(projection.bind, projection.records).await?;
        let previous = self.running.replace(replacement);
        if let Some(previous) = previous {
            previous.stop().await;
        }
        Ok(())
    }

    async fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop().await;
        }
    }
}

struct RunningResolver {
    bind: SocketAddr,
    records: InternalDnsRecords,
    shutdown: broadcast::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningResolver {
    async fn start(
        bind: SocketAddr,
        records: BTreeMap<InternalServiceName, Vec<Ipv4Addr>>,
    ) -> Result<Self, String> {
        let readiness_name = InternalServiceName::try_new(INTERNAL_DNS_READINESS_NAME)
            .map_err(|error| error.to_string())?;
        let resolver_records = InternalDnsRecords::default();
        resolver_records.replace(records_with_readiness(records)?);
        let (shutdown, receiver) = broadcast::channel(1);
        let health = InternalResolverHealth::awaiting_bind();
        let task =
            spawn_internal_resolver(resolver_records.clone(), bind, receiver, health.clone());

        let ready = tokio::time::timeout(READINESS_TIMEOUT, async {
            let Some(bound) = health.await_bound(READINESS_TIMEOUT).await else {
                return Err("resolver did not bind before its readiness deadline".to_owned());
            };
            let addresses = query_bound_resolver(bound, &readiness_name)
                .await
                .map_err(|error| error.to_string())?;
            if addresses != [INTERNAL_DNS_READINESS_ADDRESS] {
                return Err(
                    "resolver readiness response did not contain its probe address".to_owned(),
                );
            }
            Ok(())
        })
        .await
        .map_err(|_| "resolver readiness probe timed out".to_owned())?;
        if let Err(error) = ready {
            let _ = shutdown.send(());
            let _ = task.await;
            return Err(error);
        }
        tracing::info!(address = %bind, "internal DNS resolver ready");
        Ok(Self {
            bind,
            records: resolver_records,
            shutdown,
            task,
        })
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        if let Err(error) = self.task.await
            && !error.is_cancelled()
        {
            tracing::warn!(error = %error, "internal DNS resolver task failed during shutdown");
        }
    }
}

fn records_with_readiness(
    mut records: BTreeMap<InternalServiceName, Vec<Ipv4Addr>>,
) -> Result<BTreeMap<InternalServiceName, Vec<Ipv4Addr>>, String> {
    let readiness_name = InternalServiceName::try_new(INTERNAL_DNS_READINESS_NAME)
        .map_err(|error| error.to_string())?;
    records.insert(readiness_name, vec![INTERNAL_DNS_READINESS_ADDRESS]);
    Ok(records)
}

async fn retry_failure(
    retry: &mut RefreshRetry,
    shutdown: &mut watch::Receiver<bool>,
    error: impl Into<DnsRefreshError>,
) -> Result<(), DnsRoleRuntimeError> {
    let error = error.into();
    let delay = retry.record_failure(&error)?;
    tracing::warn!(error = %error, ?delay, "DNS projection refresh will retry");
    let _ = sleep_or_shutdown(shutdown, delay).await;
    Ok(())
}

enum RefreshAvailability {
    AwaitingFirstProjection { consecutive_failures: u8 },
    ServingLastKnownGood,
}

struct RefreshRetry {
    availability: RefreshAvailability,
    delay: RetryDelay,
}

impl RefreshRetry {
    const fn awaiting_first_projection() -> Self {
        Self {
            availability: RefreshAvailability::AwaitingFirstProjection {
                consecutive_failures: 0,
            },
            delay: RetryDelay::new(),
        }
    }

    fn record_success(&mut self) {
        self.availability = RefreshAvailability::ServingLastKnownGood;
        self.delay.reset();
    }

    fn record_failure(&mut self, error: &DnsRefreshError) -> Result<Duration, DnsRoleRuntimeError> {
        match &mut self.availability {
            RefreshAvailability::AwaitingFirstProjection {
                consecutive_failures,
            } => {
                *consecutive_failures = consecutive_failures.saturating_add(1);
                if *consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    return Err(DnsRoleRuntimeError::RefreshExhausted {
                        attempts: *consecutive_failures,
                        detail: error.to_string(),
                    });
                }
            }
            RefreshAvailability::ServingLastKnownGood => {}
        }
        Ok(self.delay.next())
    }
}

enum ShutdownOutcome<T> {
    Completed(T),
    Stopped,
}

async fn until_shutdown<T>(
    shutdown: &mut watch::Receiver<bool>,
    future: impl Future<Output = T>,
) -> ShutdownOutcome<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            value = &mut future => return ShutdownOutcome::Completed(value),
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return ShutdownOutcome::Stopped;
                }
            }
        }
    }
}

async fn sleep_or_shutdown(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    matches!(
        until_shutdown(shutdown, tokio::time::sleep(delay)).await,
        ShutdownOutcome::Stopped
    )
}

struct RetryDelay(Duration);

impl RetryDelay {
    const fn new() -> Self {
        Self(REFRESH_RETRY_INITIAL)
    }

    fn next(&mut self) -> Duration {
        let current = self.0;
        self.0 = self.0.saturating_mul(2).min(REFRESH_RETRY_CAP);
        current
    }

    fn reset(&mut self) {
        self.0 = REFRESH_RETRY_INITIAL;
    }
}

#[derive(Debug, thiserror::Error)]
enum DnsRefreshError {
    #[error(transparent)]
    Source(#[from] DnsSourceError),
    #[error("DNS row projection failed: {0}")]
    Projection(String),
    #[error("DNS resolver replacement failed: {0}")]
    Resolver(String),
}

#[derive(Debug, thiserror::Error)]
pub enum DnsRoleRuntimeError {
    #[error(transparent)]
    Configuration(DnsRoleConfigError),
    #[error(transparent)]
    CorrosionConfiguration(crate::corrosion::CorrosionClientConfigError),
    #[error("DNS refresh exhausted its {attempts} consecutive attempts: {detail}")]
    RefreshExhausted { attempts: u8, detail: String },
}

async fn wait_for_process_shutdown() {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
        match (terminate, interrupt) {
            (Ok(mut terminate), Ok(mut interrupt)) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
            }
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(error = %error, "could not install DNS shutdown signal handler");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket;

    fn service_name() -> InternalServiceName {
        InternalServiceName::try_new("api.default.internal").expect("service name")
    }

    fn projection(bind: SocketAddr, address: Ipv4Addr) -> InternalDnsRowProjection {
        InternalDnsRowProjection {
            bind,
            records: BTreeMap::from([(service_name(), vec![address])]),
        }
    }

    async fn available_bind() -> SocketAddr {
        let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve UDP bind");
        let bind = reservation.local_addr().expect("reserved bind");
        drop(reservation);
        bind
    }

    async fn assert_answer(bind: SocketAddr, expected: Ipv4Addr) {
        assert_eq!(
            query_bound_resolver(bind, &service_name())
                .await
                .expect("resolver answer"),
            [expected]
        );
    }

    async fn assert_readiness_answer(bind: SocketAddr) {
        let name = InternalServiceName::try_new(INTERNAL_DNS_READINESS_NAME)
            .expect("readiness service name");
        assert_eq!(
            query_bound_resolver(bind, &name)
                .await
                .expect("readiness answer"),
            [INTERNAL_DNS_READINESS_ADDRESS]
        );
    }

    #[test]
    fn retry_delay_is_bounded_and_resettable() {
        let mut retry = RetryDelay::new();
        assert_eq!(retry.next(), REFRESH_RETRY_INITIAL);
        for _ in 0..20 {
            let _ = retry.next();
        }
        assert_eq!(retry.next(), REFRESH_RETRY_CAP);
        retry.reset();
        assert_eq!(retry.next(), REFRESH_RETRY_INITIAL);
    }

    #[tokio::test]
    async fn controlled_shutdown_interrupts_retry_delay() {
        let (sender, mut shutdown) = watch::channel(false);
        let task =
            tokio::spawn(
                async move { sleep_or_shutdown(&mut shutdown, Duration::from_secs(60)).await },
            );
        sender.send(true).expect("shutdown");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), task)
                .await
                .expect("bounded shutdown")
                .expect("task")
        );
    }

    #[tokio::test]
    async fn initial_publish_and_same_bind_apply_replace_the_complete_record_snapshot() {
        let bind = available_bind().await;
        let first = Ipv4Addr::new(10, 210, 1, 10);
        let second = Ipv4Addr::new(10, 210, 1, 11);
        let mut resolver = ResolverRuntime::default();

        resolver
            .apply(projection(bind, first))
            .await
            .expect("initial publish");
        assert_answer(bind, first).await;
        assert_readiness_answer(bind).await;
        resolver
            .apply(projection(bind, second))
            .await
            .expect("same-bind replacement");
        assert_answer(bind, second).await;
        assert_readiness_answer(bind).await;

        resolver.stop().await;
    }

    #[tokio::test]
    async fn gateway_rebind_serves_the_new_socket_and_joins_the_old_resolver() {
        let old_bind = available_bind().await;
        let new_bind = available_bind().await;
        let old_address = Ipv4Addr::new(10, 210, 1, 10);
        let new_address = Ipv4Addr::new(10, 210, 2, 10);
        let mut resolver = ResolverRuntime::default();
        resolver
            .apply(projection(old_bind, old_address))
            .await
            .expect("old resolver");

        resolver
            .apply(projection(new_bind, new_address))
            .await
            .expect("new resolver");

        assert_answer(new_bind, new_address).await;
        let rebound = UdpSocket::bind(old_bind)
            .await
            .expect("old resolver socket was released before apply returned");
        drop(rebound);
        resolver.stop().await;
    }

    #[tokio::test]
    async fn failed_rebind_retains_the_last_known_good_binding_and_records() {
        let old_bind = available_bind().await;
        let blocked = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("block replacement bind");
        let blocked_bind = blocked.local_addr().expect("blocked bind");
        let old_address = Ipv4Addr::new(10, 210, 1, 10);
        let mut resolver = ResolverRuntime::default();
        resolver
            .apply(projection(old_bind, old_address))
            .await
            .expect("old resolver");

        let error = resolver
            .apply(projection(blocked_bind, Ipv4Addr::new(10, 210, 2, 10)))
            .await
            .expect_err("occupied replacement bind must fail readiness");

        assert!(error.contains("readiness"));
        assert_answer(old_bind, old_address).await;
        drop(blocked);
        resolver.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn retry_budget_returns_a_typed_exhaustion_error() {
        let (_sender, mut shutdown) = watch::channel(false);
        let mut retry = RefreshRetry::awaiting_first_projection();

        for attempt in 1..MAX_CONSECUTIVE_FAILURES {
            retry_failure(
                &mut retry,
                &mut shutdown,
                DnsRefreshError::Projection(format!("failure {attempt}")),
            )
            .await
            .expect("retry remains in budget");
        }
        let error = retry_failure(
            &mut retry,
            &mut shutdown,
            DnsRefreshError::Projection("exhausted".to_owned()),
        )
        .await
        .expect_err("retry budget exhausted");

        assert!(matches!(
            error,
            DnsRoleRuntimeError::RefreshExhausted { attempts, detail }
                if attempts == MAX_CONSECUTIVE_FAILURES && detail.contains("exhausted")
        ));
    }

    #[tokio::test]
    async fn last_known_good_survives_the_startup_budget_recovers_and_stops_promptly() {
        let bind = available_bind().await;
        let first = Ipv4Addr::new(10, 210, 1, 10);
        let recovered = Ipv4Addr::new(10, 210, 1, 11);
        let mut resolver = ResolverRuntime::default();
        resolver
            .apply(projection(bind, first))
            .await
            .expect("initial projection");
        let mut retry = RefreshRetry::awaiting_first_projection();
        retry.record_success();

        for attempt in 1..=usize::from(MAX_CONSECUTIVE_FAILURES) + 2 {
            retry
                .record_failure(&DnsRefreshError::Projection(format!("failure {attempt}")))
                .expect("a serving resolver has no terminal refresh budget");
        }
        assert_answer(bind, first).await;

        resolver
            .apply(projection(bind, recovered))
            .await
            .expect("refresh recovers");
        retry.record_success();
        assert_answer(bind, recovered).await;

        let (sender, mut shutdown) = watch::channel(false);
        let task = tokio::spawn(async move {
            retry_failure(
                &mut retry,
                &mut shutdown,
                DnsRefreshError::Projection("after recovery".to_owned()),
            )
            .await
        });
        tokio::task::yield_now().await;
        sender.send(true).expect("shutdown");
        tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .expect("shutdown interrupts last-known-good retry")
            .expect("retry task")
            .expect("shutdown is not a refresh failure");
        resolver.stop().await;
    }

    #[tokio::test]
    async fn stop_joins_the_resolver_before_returning() {
        let bind = available_bind().await;
        let mut resolver = ResolverRuntime::default();
        resolver
            .apply(projection(bind, Ipv4Addr::new(10, 210, 1, 10)))
            .await
            .expect("resolver");

        resolver.stop().await;

        let rebound = UdpSocket::bind(bind)
            .await
            .expect("joined resolver released the UDP socket");
        drop(rebound);
    }
}
