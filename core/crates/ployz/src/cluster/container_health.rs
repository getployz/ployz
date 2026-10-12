//! Health wait for a started Container, shared by Deploy, restart and `ployz service start`.

use std::{future::Future, time::Duration};

use ployz_core::{
    ContainerDetails, ContainerObservation, ContainerRuntimeObservation, HealthFailure,
    HealthObservation, HealthcheckSpec, LastHealthCheck, healthcheck_deadline,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const DEFAULT_HEALTH_MONITOR: Duration = Duration::from_secs(5);

pub(crate) enum HealthPoll {
    Complete,
    PendingUntil(Instant),
    Failed(HealthFailure),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum HealthExpectation {
    ContainerReady,
    Healthy,
}

#[derive(Debug)]
pub(crate) enum HealthWaitError<E> {
    Inspect(E),
    Health(HealthFailure),
}

/// One look at a Container the wait is still holding for.
pub(crate) struct HealthWaitTick {
    pub(crate) health: Option<HealthObservation>,
    pub(crate) deadline: Instant,
}

/// Poll a started Container until it is ready, fails, or outlives its health deadline.
///
/// `healthcheck` is the Container's spec healthcheck when the caller has it. Without
/// one the deadline comes from the healthcheck the Machine observed, then the default.
pub(crate) async fn wait_until_ready<E, F: Future<Output = Result<ContainerDetails, E>>>(
    healthcheck: Option<&HealthcheckSpec>,
    monitor: Duration,
    started: Instant,
    cancellation: &CancellationToken,
    mut inspect: impl FnMut() -> F,
    mut tick: impl FnMut(HealthWaitTick),
) -> Result<(), HealthWaitError<E>> {
    let monitor_deadline = started + monitor;
    loop {
        if cancellation.is_cancelled() {
            return Err(HealthWaitError::Health(HealthFailure::Cancelled));
        }
        let ContainerDetails {
            container: observed,
            last_check,
            ..
        } = inspect().await.map_err(HealthWaitError::Inspect)?;
        let now = Instant::now();
        let health_deadline = health_deadline_for(healthcheck, &observed, started);
        tick(HealthWaitTick {
            health: observed_health(&observed.runtime),
            deadline: health_deadline.unwrap_or(monitor_deadline),
        });
        let wake_deadline = match classify_health(
            &observed.runtime,
            last_check.as_ref(),
            now,
            if matches!(healthcheck, Some(HealthcheckSpec::Http(_))) {
                health_deadline.unwrap_or(monitor_deadline)
            } else {
                monitor_deadline
            },
            health_deadline,
            HealthExpectation::ContainerReady,
        ) {
            HealthPoll::Complete => return Ok(()),
            HealthPoll::PendingUntil(deadline) => deadline,
            HealthPoll::Failed(failure) => return Err(HealthWaitError::Health(failure)),
        };
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(HealthWaitError::Health(HealthFailure::Cancelled));
            }
            () = tokio::time::sleep_until(std::cmp::min(now + POLL_INTERVAL, wake_deadline)) => {}
        }
    }
}

pub(crate) fn classify_health(
    runtime: &ContainerRuntimeObservation,
    last_check: Option<&LastHealthCheck>,
    now: Instant,
    monitor_deadline: Instant,
    health_deadline: Option<Instant>,
    expectation: HealthExpectation,
) -> HealthPoll {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy | HealthObservation::Failing,
        } => HealthPoll::Complete,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::NotConfigured,
        } if expectation == HealthExpectation::ContainerReady && health_deadline.is_none() => {
            HealthPoll::Complete
        }
        ContainerRuntimeObservation::Running {
            health:
                HealthObservation::Starting
                | HealthObservation::NotConfigured
                | HealthObservation::Unrecognized(_),
        } => {
            let Some(health_deadline) = health_deadline else {
                return HealthPoll::Failed(HealthFailure::Runtime {
                    observation: runtime.clone(),
                    last_check: last_check.cloned(),
                });
            };
            if now >= health_deadline {
                HealthPoll::Failed(HealthFailure::TimedOut {
                    last_check: last_check.cloned(),
                })
            } else {
                HealthPoll::PendingUntil(health_deadline)
            }
        }
        // The process already died. Waiting would let a later healthy snapshot
        // during the next start window succeed the deploy.
        ContainerRuntimeObservation::Restarting | ContainerRuntimeObservation::Exited { .. }
            if expectation == HealthExpectation::ContainerReady =>
        {
            HealthPoll::Failed(HealthFailure::Runtime {
                observation: runtime.clone(),
                last_check: last_check.cloned(),
            })
        }
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy | HealthObservation::Stopping,
        }
        | ContainerRuntimeObservation::Restarting
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Dead
        | ContainerRuntimeObservation::Unknown { .. } => {
            if now >= monitor_deadline {
                HealthPoll::Failed(HealthFailure::Runtime {
                    observation: runtime.clone(),
                    last_check: last_check.cloned(),
                })
            } else {
                HealthPoll::PendingUntil(monitor_deadline)
            }
        }
    }
}

pub(crate) fn observed_health(runtime: &ContainerRuntimeObservation) -> Option<HealthObservation> {
    match runtime {
        ContainerRuntimeObservation::Running { health } => Some(health.clone()),
        ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Restarting
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Dead
        | ContainerRuntimeObservation::Unknown { .. } => None,
    }
}

pub(crate) fn health_deadline_for(
    spec: Option<&HealthcheckSpec>,
    observed: &ContainerObservation,
    started: Instant,
) -> Option<Instant> {
    match spec {
        Some(spec) => spec.deadline().map(|deadline| started + deadline),
        None => observed
            .effective_healthcheck
            .as_ref()
            .and_then(HealthcheckSpec::as_configured)
            .map(|check| started + healthcheck_deadline(Some(check)))
            .or_else(|| {
                matches!(
                    &observed.runtime,
                    ContainerRuntimeObservation::Running {
                        health: HealthObservation::Starting | HealthObservation::Unrecognized(_),
                    }
                )
                .then(|| started + healthcheck_deadline(None))
            }),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{ConfiguredHealthcheck, HealthcheckCommand};

    use ployz_core::RpcError;

    use super::*;
    use crate::global_fixtures::{machine, running_on};

    fn details(
        health: HealthObservation,
        effective_healthcheck: Option<HealthcheckSpec>,
    ) -> ContainerDetails {
        let mut container = running_on(&machine('a', "one"), '1');
        container
            .try_update(|parts| {
                parts.runtime = ContainerRuntimeObservation::Running { health };
                parts.effective_healthcheck = effective_healthcheck;
            })
            .unwrap();
        ContainerDetails {
            container,
            environment: None,
            image_id: None,
            last_check: None,
        }
    }

    async fn wait(
        observe: impl Fn(Duration) -> ContainerDetails,
    ) -> (Result<(), HealthWaitError<RpcError>>, Duration) {
        let started = Instant::now();
        let result = wait_until_ready(
            None,
            DEFAULT_HEALTH_MONITOR,
            started,
            &CancellationToken::new(),
            || std::future::ready(Ok(observe(started.elapsed()))),
            |_| {},
        )
        .await;
        (result, started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn a_container_that_turns_healthy_after_the_serving_wait_is_ready() {
        let (result, waited) = wait(|elapsed| {
            details(
                if elapsed < Duration::from_secs(45) {
                    HealthObservation::Starting
                } else {
                    HealthObservation::Healthy
                },
                None,
            )
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(waited, Duration::from_secs(45));
    }

    #[tokio::test(start_paused = true)]
    async fn a_container_that_never_passes_fails_at_its_observed_healthcheck_deadline() {
        let healthcheck = HealthcheckSpec::Configured(ConfiguredHealthcheck {
            test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
            interval_millis: None,
            timeout_millis: None,
            start_period_millis: None,
            start_interval_millis: None,
            retries: None,
            deadline_millis: Some(60_000),
        });
        let (result, waited) =
            wait(|_| details(HealthObservation::Starting, Some(healthcheck.clone()))).await;
        assert!(matches!(
            result,
            Err(HealthWaitError::Health(HealthFailure::TimedOut { .. }))
        ));
        assert_eq!(waited, Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn a_container_without_an_observed_healthcheck_waits_out_the_default_deadline() {
        let (result, waited) = wait(|_| details(HealthObservation::Starting, None)).await;
        assert!(matches!(
            result,
            Err(HealthWaitError::Health(HealthFailure::TimedOut { .. }))
        ));
        assert_eq!(waited, healthcheck_deadline(None));
    }
}
