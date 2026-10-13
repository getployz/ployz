use std::future::Future;

use ployz_core::{
    ContainerDetails, ContainerId, DEFAULT_HEALTH_MONITOR, HEALTH_POLL_INTERVAL, HealthFailure,
    HealthPoll, InspectContainerRequest, MachineId, MachineTarget, RpcError, op, ready_poll,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::Client;
use crate::connect::TARGET_RPC_TIMEOUT;

#[derive(Debug)]
pub(crate) enum HealthWaitError<E> {
    Inspect(E),
    Health(HealthFailure),
}

async fn wait_until_ready<E, F: Future<Output = Result<ContainerDetails, E>>>(
    started: Instant,
    cancellation: &CancellationToken,
    mut inspect: impl FnMut() -> F,
) -> Result<(), HealthWaitError<E>> {
    loop {
        if cancellation.is_cancelled() {
            return Err(HealthWaitError::Health(HealthFailure::Cancelled));
        }
        let details = inspect().await.map_err(HealthWaitError::Inspect)?;
        let elapsed = started.elapsed();
        let wake = match ready_poll(None, DEFAULT_HEALTH_MONITOR, &details, elapsed).0 {
            HealthPoll::Complete => return Ok(()),
            HealthPoll::PendingUntil(deadline) => deadline,
            HealthPoll::Failed(failure) => return Err(HealthWaitError::Health(failure)),
        };
        tokio::select! {
            () = cancellation.cancelled() => {
                return Err(HealthWaitError::Health(HealthFailure::Cancelled));
            }
            () = tokio::time::sleep_until(started + std::cmp::min(elapsed + HEALTH_POLL_INTERVAL, wake)) => {}
        }
    }
}

impl Client {
    /// Wait for Container `container_id` on `machine_id`, started just now, to be ready.
    pub(crate) async fn wait_until_ready(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        cancellation: &CancellationToken,
    ) -> Result<(), HealthWaitError<RpcError>> {
        let target = MachineTarget::from(machine_id);
        wait_until_ready(Instant::now(), cancellation, || {
            self.invoke::<op::InspectContainer>(
                InspectContainerRequest {
                    container_id: *container_id,
                },
                &target,
                Some(TARGET_RPC_TIMEOUT),
            )
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU16, time::Duration};

    use ployz_core::{
        ConfiguredHealthcheck, ContainerRuntimeObservation, HealthObservation, HealthcheckCommand,
        HealthcheckSpec, HttpHealthcheck, healthcheck_deadline,
    };

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
        let result = wait_until_ready(started, &CancellationToken::new(), || {
            std::future::ready(Ok(observe(started.elapsed())))
        })
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

    #[tokio::test(start_paused = true)]
    async fn a_container_without_a_healthcheck_is_ready_once_it_outlives_the_monitor_window() {
        let (result, waited) = wait(|_| details(HealthObservation::NotConfigured, None)).await;
        assert!(result.is_ok());
        assert_eq!(waited, DEFAULT_HEALTH_MONITOR);
    }

    #[tokio::test(start_paused = true)]
    async fn a_container_without_a_healthcheck_that_dies_inside_the_monitor_window_fails() {
        let (result, waited) = wait(|elapsed| {
            let mut details = details(HealthObservation::NotConfigured, None);
            if elapsed >= Duration::from_secs(2) {
                details
                    .container
                    .try_update(|parts| {
                        parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
                    })
                    .unwrap();
            }
            details
        })
        .await;
        assert!(matches!(
            result,
            Err(HealthWaitError::Health(HealthFailure::Runtime { .. }))
        ));
        assert_eq!(waited, Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn an_observed_http_healthcheck_sets_the_deadline() {
        let healthcheck = HealthcheckSpec::Http(HttpHealthcheck {
            path: "/health".into(),
            port: NonZeroU16::new(8080).unwrap(),
            timeout_seconds: 300,
        });
        let (result, waited) = wait(|_| {
            details(
                HealthObservation::Unrecognized("probe address unavailable".into()),
                Some(healthcheck.clone()),
            )
        })
        .await;
        assert!(matches!(
            result,
            Err(HealthWaitError::Health(HealthFailure::TimedOut { .. }))
        ));
        assert_eq!(waited, Duration::from_secs(300));
    }
}
