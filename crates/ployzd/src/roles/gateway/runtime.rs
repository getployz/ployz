//! Gateway lifecycle: complete projection refresh, atomic serving, and shutdown.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use pingora::server::configuration::ServerConf;
use pingora::services::Service;
use tokio::sync::{oneshot, watch};

use crate::corrosion::CorrosionClient;

use super::config::{GatewayRoleConfig, GatewayRoleConfigError};
use super::pingora::{PingoraRouteRegistry, PloyzGatewayProxy};
use super::projection::{GatewayProjectionInput, project_gateway_rows};
use super::source::{CorrosionGatewaySource, GatewayRows, GatewaySourceError};

const REFRESH_RETRY_INITIAL: Duration = Duration::from_millis(250);
const REFRESH_RETRY_CAP: Duration = Duration::from_secs(5);
const MAX_CONSECUTIVE_STARTUP_FAILURES: u8 = 8;
const INVALIDATION_COALESCE: Duration = Duration::from_millis(50);

pub async fn run_from_environment() -> Result<(), GatewayRoleRuntimeError> {
    let config =
        GatewayRoleConfig::from_environment().map_err(GatewayRoleRuntimeError::Configuration)?;
    let client = CorrosionClient::new(config.corrosion().clone())
        .map_err(GatewayRoleRuntimeError::CorrosionConfiguration)?;
    let source = CorrosionGatewaySource::new(client, config.cluster_id().clone());
    let (shutdown_tx, shutdown) = watch::channel(false);
    let signal = tokio::spawn(async move {
        wait_for_process_shutdown().await;
        let _ = shutdown_tx.send(true);
    });
    let result = run_gateway(config, source, shutdown).await;
    signal.abort();
    let _ = signal.await;
    result
}

async fn run_gateway(
    config: GatewayRoleConfig,
    source: CorrosionGatewaySource,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), GatewayRoleRuntimeError> {
    let registry = PingoraRouteRegistry::new();
    let (ready_tx, ready_rx) = oneshot::channel();
    let mut projection_task = tokio::spawn(run_projection_loop(
        config.cluster_id().clone(),
        source,
        registry.clone(),
        shutdown.clone(),
        ready_tx,
    ));

    tokio::select! {
        ready = ready_rx => ready.map_err(|_| GatewayRoleRuntimeError::ProjectionStoppedBeforeReady)?,
        result = &mut projection_task => return projection_result(result),
        changed = shutdown.changed() => {
            let _ = changed;
            projection_task.abort();
            let _ = projection_task.await;
            return Ok(());
        }
    }

    let listen_addr = config.listen_addr();
    let mut listener_task = tokio::spawn(run_listener(listen_addr, registry, shutdown.clone()));
    tracing::info!(address = %listen_addr, "public HTTP gateway ready");

    let outcome = tokio::select! {
        result = &mut projection_task => projection_result(result),
        result = &mut listener_task => listener_result(result),
        changed = shutdown.changed() => {
            let _ = changed;
            Ok(())
        }
    };

    projection_task.abort();
    listener_task.abort();
    let _ = projection_task.await;
    let _ = listener_task.await;
    outcome
}

async fn run_listener(
    listen_addr: std::net::SocketAddr,
    registry: PingoraRouteRegistry,
    shutdown: watch::Receiver<bool>,
) -> Result<(), GatewayRoleRuntimeError> {
    let configuration = Arc::new(ServerConf::default());
    let mut service =
        pingora::proxy::http_proxy_service(&configuration, PloyzGatewayProxy::new(registry));
    service.add_tcp(&listen_addr.to_string());
    Service::start_service(&mut service, None, shutdown.clone(), 1).await;
    if *shutdown.borrow() {
        Ok(())
    } else {
        Err(GatewayRoleRuntimeError::ListenerStopped)
    }
}

async fn run_projection_loop(
    cluster_id: ployz_core::ids::ClusterId,
    source: CorrosionGatewaySource,
    registry: PingoraRouteRegistry,
    mut shutdown: watch::Receiver<bool>,
    ready: oneshot::Sender<()>,
) -> Result<(), GatewayRoleRuntimeError> {
    let mut subscriptions = None;
    let mut retry = RefreshRetry::new();
    let mut ready = Some(ready);

    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        if subscriptions.is_none() {
            match until_shutdown(&mut shutdown, source.subscribe()).await {
                ShutdownOutcome::Stopped => return Ok(()),
                ShutdownOutcome::Completed(Ok(active)) => subscriptions = Some(active),
                ShutdownOutcome::Completed(Err(error)) => {
                    retry_failure(&mut retry, &mut shutdown, error).await?;
                    continue;
                }
            }
        }

        let rows = match until_shutdown(&mut shutdown, source.query_rows()).await {
            ShutdownOutcome::Stopped => return Ok(()),
            ShutdownOutcome::Completed(Ok(rows)) => rows,
            ShutdownOutcome::Completed(Err(error)) => {
                retry_failure(&mut retry, &mut shutdown, error).await?;
                continue;
            }
        };
        registry.replace_projection(&project_rows(cluster_id.clone(), rows));
        retry.record_success();
        if let Some(ready) = ready.take() {
            let _ = ready.send(());
        }

        let Some(active) = subscriptions.as_mut() else {
            continue;
        };
        match until_shutdown(&mut shutdown, active.wait_for_invalidation()).await {
            ShutdownOutcome::Stopped => return Ok(()),
            ShutdownOutcome::Completed(Ok(())) => {
                if sleep_or_shutdown(&mut shutdown, INVALIDATION_COALESCE).await {
                    return Ok(());
                }
                if let Err(error) = active.drain_ready_invalidations().await {
                    subscriptions = None;
                    retry_failure(&mut retry, &mut shutdown, error).await?;
                }
            }
            ShutdownOutcome::Completed(Err(error)) => {
                subscriptions = None;
                retry_failure(&mut retry, &mut shutdown, error).await?;
            }
        }
    }
}

fn project_rows(
    cluster_id: ployz_core::ids::ClusterId,
    rows: GatewayRows,
) -> super::projection::GatewayProjection {
    project_gateway_rows(GatewayProjectionInput {
        cluster_id,
        services: rows.services,
        route_bindings: rows.route_bindings,
        containers: rows.containers,
    })
}

async fn retry_failure(
    retry: &mut RefreshRetry,
    shutdown: &mut watch::Receiver<bool>,
    error: GatewaySourceError,
) -> Result<(), GatewayRoleRuntimeError> {
    let delay = retry.record_failure(&error)?;
    tracing::warn!(error = %error, ?delay, "gateway projection refresh will retry");
    let _ = sleep_or_shutdown(shutdown, delay).await;
    Ok(())
}

struct RefreshRetry {
    serving_last_known_good: bool,
    consecutive_startup_failures: u8,
    delay: Duration,
}

impl RefreshRetry {
    const fn new() -> Self {
        Self {
            serving_last_known_good: false,
            consecutive_startup_failures: 0,
            delay: REFRESH_RETRY_INITIAL,
        }
    }

    fn record_success(&mut self) {
        self.serving_last_known_good = true;
        self.consecutive_startup_failures = 0;
        self.delay = REFRESH_RETRY_INITIAL;
    }

    fn record_failure(
        &mut self,
        error: &GatewaySourceError,
    ) -> Result<Duration, GatewayRoleRuntimeError> {
        if !self.serving_last_known_good {
            self.consecutive_startup_failures = self.consecutive_startup_failures.saturating_add(1);
            if self.consecutive_startup_failures >= MAX_CONSECUTIVE_STARTUP_FAILURES {
                return Err(GatewayRoleRuntimeError::RefreshExhausted {
                    attempts: self.consecutive_startup_failures,
                    detail: error.to_string(),
                });
            }
        }
        let current = self.delay;
        self.delay = self.delay.saturating_mul(2).min(REFRESH_RETRY_CAP);
        Ok(current)
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

fn projection_result(
    result: Result<Result<(), GatewayRoleRuntimeError>, tokio::task::JoinError>,
) -> Result<(), GatewayRoleRuntimeError> {
    result.map_err(|error| GatewayRoleRuntimeError::ProjectionTask {
        detail: error.to_string(),
    })?
}

fn listener_result(
    result: Result<Result<(), GatewayRoleRuntimeError>, tokio::task::JoinError>,
) -> Result<(), GatewayRoleRuntimeError> {
    result.map_err(|error| GatewayRoleRuntimeError::ListenerTask {
        detail: error.to_string(),
    })?
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayRoleRuntimeError {
    #[error(transparent)]
    Configuration(GatewayRoleConfigError),
    #[error(transparent)]
    CorrosionConfiguration(crate::corrosion::CorrosionClientConfigError),
    #[error("gateway refresh exhausted its {attempts} consecutive startup attempts: {detail}")]
    RefreshExhausted { attempts: u8, detail: String },
    #[error("gateway projection task stopped before publishing its first snapshot")]
    ProjectionStoppedBeforeReady,
    #[error("gateway projection task failed: {detail}")]
    ProjectionTask { detail: String },
    #[error("gateway listener task failed: {detail}")]
    ListenerTask { detail: String },
    #[error("gateway listener stopped without process shutdown")]
    ListenerStopped,
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
                tracing::warn!(error = %error, "could not install gateway shutdown signal handler");
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

    #[test]
    fn pre_first_projection_failures_are_bounded_but_last_known_good_is_indefinite() {
        let error = GatewaySourceError::SubscriptionsEnded;
        let mut cold = RefreshRetry::new();
        for _ in 1..MAX_CONSECUTIVE_STARTUP_FAILURES {
            assert!(cold.record_failure(&error).is_ok());
        }
        assert!(matches!(
            cold.record_failure(&error),
            Err(GatewayRoleRuntimeError::RefreshExhausted { .. })
        ));

        let mut warm = RefreshRetry::new();
        warm.record_success();
        for _ in 0..64 {
            assert!(warm.record_failure(&error).is_ok());
        }
    }

    #[test]
    fn refresh_coalescing_never_exceeds_one_hundred_milliseconds() {
        assert!(INVALIDATION_COALESCE <= Duration::from_millis(100));
    }
}
