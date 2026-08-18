//! Health-monitor wait for a started container.

use std::time::Duration;

use ployz_core::{
    ConfiguredHealthcheck, ContainerId, ContainerObservation, ContainerRuntimeObservation,
    ExecutionError, HealthFailure, HealthObservation, HealthcheckSpec, MachineId, OperationPhase,
    ResolvedServiceSpec,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{MachineOperations, POLL_INTERVAL, Progress, inspect};

const DEFAULT_HEALTH_MONITOR: Duration = Duration::from_secs(5);

enum HealthPoll {
    Complete,
    PendingUntil(Instant),
    Failed(HealthFailure),
}

pub(super) async fn monitor_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &mut Progress,
    machine_id: &MachineId,
    container_id: &ContainerId,
    spec: &ResolvedServiceSpec,
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    let monitor = spec
        .update
        .monitor_millis
        .map_or_else(default_health_monitor, Duration::from_millis);
    let started = Instant::now();
    let deadline_ms = healthcheck_timeout(
        spec.container
            .healthcheck
            .as_ref()
            .and_then(HealthcheckSpec::as_configured),
    )
    .as_millis() as u64;
    progress.set_running(
        index,
        OperationPhase::WaitingForHealth {
            container_id: *container_id,
            health: None,
            elapsed_ms: 0,
            deadline_ms,
        },
    );

    if !matches!(
        spec.container.healthcheck,
        Some(HealthcheckSpec::Configured(_))
    ) && !monitor.is_zero()
    {
        let deadline = started + monitor;
        loop {
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(health_error(container_id, HealthFailure::Cancelled));
                }
                () = tokio::time::sleep_until(std::cmp::min(Instant::now() + POLL_INTERVAL, deadline)) => {}
            }
            progress.set_running(
                index,
                OperationPhase::WaitingForHealth {
                    container_id: *container_id,
                    health: None,
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    deadline_ms,
                },
            );
            if Instant::now() >= deadline {
                break;
            }
        }
    }

    let monitor_deadline = started + monitor;
    loop {
        if cancellation.is_cancelled() {
            return Err(health_error(container_id, HealthFailure::Cancelled));
        }
        let observed = inspect(client, machine_id, container_id).await?;
        let now = Instant::now();
        let health_deadline =
            health_deadline_for(spec.container.healthcheck.as_ref(), &observed, started);
        let health = match &observed.runtime {
            ContainerRuntimeObservation::Running { health } => Some(health.clone()),
            ContainerRuntimeObservation::Created
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
            | ContainerRuntimeObservation::Exited { .. }
            | ContainerRuntimeObservation::Removing
            | ContainerRuntimeObservation::Dead
            | ContainerRuntimeObservation::Unknown { .. } => None,
        };
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let deadline_ms = health_deadline
            .or(Some(monitor_deadline))
            .map(|deadline| {
                u64::try_from(deadline.saturating_duration_since(started).as_millis())
                    .unwrap_or(u64::MAX)
            })
            .unwrap_or(deadline_ms);
        progress.set_running(
            index,
            OperationPhase::WaitingForHealth {
                container_id: *container_id,
                health,
                elapsed_ms,
                deadline_ms,
            },
        );
        let wake_deadline =
            match classify_health(&observed.runtime, now, monitor_deadline, health_deadline) {
                HealthPoll::Complete => return Ok(()),
                HealthPoll::PendingUntil(deadline) => deadline,
                HealthPoll::Failed(failure) => return Err(health_error(container_id, failure)),
            };
        let wake = std::cmp::min(now + POLL_INTERVAL, wake_deadline);
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(health_error(container_id, HealthFailure::Cancelled));
            }
            () = tokio::time::sleep_until(wake) => {}
        }
    }
}

fn classify_health(
    runtime: &ContainerRuntimeObservation,
    now: Instant,
    monitor_deadline: Instant,
    health_deadline: Option<Instant>,
) -> HealthPoll {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        } => HealthPoll::Complete,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::NotConfigured,
        } if health_deadline.is_none() => HealthPoll::Complete,
        ContainerRuntimeObservation::Exited { code: 0 } => HealthPoll::Complete,
        ContainerRuntimeObservation::Running {
            health:
                HealthObservation::Starting
                | HealthObservation::NotConfigured
                | HealthObservation::Unrecognized(_),
        } => {
            let Some(health_deadline) = health_deadline else {
                return HealthPoll::Failed(HealthFailure::Runtime {
                    observation: runtime.clone(),
                });
            };
            if now >= health_deadline {
                HealthPoll::Failed(HealthFailure::TimedOut)
            } else {
                HealthPoll::PendingUntil(health_deadline)
            }
        }
        // The process already died. Waiting would let a later healthy snapshot
        // during the next start window succeed the deploy.
        ContainerRuntimeObservation::Restarting => HealthPoll::Failed(HealthFailure::Runtime {
            observation: runtime.clone(),
        }),
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        }
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Dead
        | ContainerRuntimeObservation::Unknown { .. } => {
            if now >= monitor_deadline {
                HealthPoll::Failed(HealthFailure::Runtime {
                    observation: runtime.clone(),
                })
            } else {
                HealthPoll::PendingUntil(monitor_deadline)
            }
        }
    }
}

fn health_error(container_id: &ContainerId, failure: HealthFailure) -> ExecutionError {
    ExecutionError::Health {
        container_id: *container_id,
        failure,
    }
}

fn health_deadline_for(
    spec: Option<&HealthcheckSpec>,
    observed: &ContainerObservation,
    started: Instant,
) -> Option<Instant> {
    match spec {
        Some(HealthcheckSpec::Configured(configured)) => {
            Some(started + healthcheck_timeout(Some(configured)))
        }
        Some(HealthcheckSpec::Disabled) => None,
        None => observed
            .effective_healthcheck
            .as_ref()
            .and_then(HealthcheckSpec::as_configured)
            .map(|check| started + healthcheck_timeout(Some(check)))
            .or_else(|| {
                matches!(
                    &observed.runtime,
                    ContainerRuntimeObservation::Running {
                        health: HealthObservation::Starting | HealthObservation::Unrecognized(_),
                    }
                )
                .then(|| started + healthcheck_timeout(None))
            }),
    }
}

fn default_health_monitor() -> Duration {
    std::env::var(crate::cli::env::HEALTH_MONITOR_PERIOD)
        .ok()
        .and_then(|value| parse_monitor_period(&value))
        .unwrap_or(DEFAULT_HEALTH_MONITOR)
}

pub(crate) fn parse_monitor_period(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value == "0" {
        return Some(Duration::ZERO);
    }
    crate::compose::duration_millis(Some(value))
        .ok()
        .flatten()
        .map(Duration::from_millis)
}

fn healthcheck_timeout(healthcheck: Option<&ConfiguredHealthcheck>) -> Duration {
    let interval = healthcheck
        .and_then(|check| check.interval_millis)
        .unwrap_or(30_000);
    let timeout = healthcheck
        .and_then(|check| check.timeout_millis)
        .unwrap_or(30_000);
    let retries = u64::from(healthcheck.and_then(|check| check.retries).unwrap_or(3));
    Duration::from_millis(
        healthcheck
            .and_then(|check| check.start_period_millis)
            .unwrap_or_default()
            .saturating_add(interval.saturating_add(timeout).saturating_mul(retries))
            .saturating_add(5_000),
    )
}
