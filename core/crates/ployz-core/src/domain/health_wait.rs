use std::time::Duration;

use crate::{
    ContainerDetails, ContainerObservation, ContainerRuntimeObservation, HealthFailure,
    HealthObservation, HealthcheckSpec, LastHealthCheck, ResolvedServiceSpec, healthcheck_deadline,
};

pub const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long a Container without a healthcheck must stay up to count as started.
pub const DEFAULT_HEALTH_MONITOR: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
pub enum HealthPoll {
    Complete,
    /// Look again before this offset from the start.
    PendingUntil(Duration),
    Failed(HealthFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthExpectation {
    ContainerReady,
    Healthy,
}

/// The window `spec` asks a Container without a healthcheck to stay up through.
#[must_use]
pub fn health_monitor(spec: &ResolvedServiceSpec) -> Duration {
    spec.update
        .monitor_millis
        .map_or(DEFAULT_HEALTH_MONITOR, Duration::from_millis)
}

/// One look at a Container waited on to be ready: the verdict so far, and the
/// offset the wait gives up at.
///
/// `healthcheck` is the Container's spec healthcheck when the caller has it.
/// Without one the deadline comes from the healthcheck the Machine observed.
#[must_use]
pub fn ready_poll(
    healthcheck: Option<&HealthcheckSpec>,
    monitor: Duration,
    details: &ContainerDetails,
    elapsed: Duration,
) -> (HealthPoll, Duration) {
    let observed = &details.container;
    let health_deadline = health_deadline_for(healthcheck, observed);
    let deadline = health_deadline.unwrap_or(monitor);
    let monitor_deadline = if matches!(
        healthcheck.or(observed.effective_healthcheck.as_ref()),
        Some(HealthcheckSpec::Http(_))
    ) {
        deadline
    } else {
        monitor
    };
    let poll = classify_health(
        &observed.runtime,
        details.last_check.as_ref(),
        elapsed,
        monitor_deadline,
        health_deadline,
        HealthExpectation::ContainerReady,
    );
    (poll, deadline)
}

#[must_use]
pub fn classify_health(
    runtime: &ContainerRuntimeObservation,
    last_check: Option<&LastHealthCheck>,
    elapsed: Duration,
    monitor_deadline: Duration,
    health_deadline: Option<Duration>,
    expectation: HealthExpectation,
) -> HealthPoll {
    match runtime {
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy | HealthObservation::Failing,
        } => HealthPoll::Complete,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::NotConfigured,
        } if expectation == HealthExpectation::ContainerReady && health_deadline.is_none() => {
            if elapsed >= monitor_deadline {
                HealthPoll::Complete
            } else {
                HealthPoll::PendingUntil(monitor_deadline)
            }
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
            if elapsed >= health_deadline {
                HealthPoll::Failed(HealthFailure::TimedOut {
                    last_check: last_check.cloned(),
                })
            } else {
                HealthPoll::PendingUntil(health_deadline)
            }
        }
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
            if elapsed >= monitor_deadline {
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

#[must_use]
pub fn observed_health(runtime: &ContainerRuntimeObservation) -> Option<HealthObservation> {
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

/// The offset from the start a first passing check is waited for, if any check applies.
#[must_use]
pub fn health_deadline_for(
    spec: Option<&HealthcheckSpec>,
    observed: &ContainerObservation,
) -> Option<Duration> {
    match spec {
        Some(spec) => spec.deadline(),
        None => observed
            .effective_healthcheck
            .as_ref()
            .and_then(HealthcheckSpec::deadline)
            .or_else(|| {
                matches!(
                    &observed.runtime,
                    ContainerRuntimeObservation::Running {
                        health: HealthObservation::Starting | HealthObservation::Unrecognized(_),
                    }
                )
                .then(|| healthcheck_deadline(None))
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Duration = Duration::from_secs(5);
    const DEADLINE: Duration = Duration::from_secs(60);

    fn running(health: HealthObservation) -> ContainerRuntimeObservation {
        ContainerRuntimeObservation::Running { health }
    }

    fn ready(
        runtime: &ContainerRuntimeObservation,
        elapsed: Duration,
        health_deadline: Option<Duration>,
    ) -> HealthPoll {
        classify_health(
            runtime,
            None,
            elapsed,
            MONITOR,
            health_deadline,
            HealthExpectation::ContainerReady,
        )
    }

    #[test]
    fn a_healthy_container_is_ready_at_once() {
        assert_eq!(
            ready(
                &running(HealthObservation::Healthy),
                Duration::ZERO,
                Some(DEADLINE)
            ),
            HealthPoll::Complete
        );
    }

    #[test]
    fn a_container_that_exits_fails_at_once() {
        assert!(matches!(
            ready(
                &ContainerRuntimeObservation::Exited { code: 1 },
                Duration::ZERO,
                Some(DEADLINE)
            ),
            HealthPoll::Failed(HealthFailure::Runtime { .. })
        ));
    }

    #[test]
    fn a_restarting_container_fails_without_waiting_out_the_monitor_window() {
        assert!(matches!(
            ready(
                &ContainerRuntimeObservation::Restarting,
                Duration::ZERO,
                Some(DEADLINE)
            ),
            HealthPoll::Failed(HealthFailure::Runtime { .. })
        ));
    }

    #[test]
    fn a_check_that_passed_once_and_is_failing_now_is_ready() {
        assert_eq!(
            ready(
                &running(HealthObservation::Failing),
                Duration::ZERO,
                Some(DEADLINE)
            ),
            HealthPoll::Complete
        );
    }

    #[test]
    fn an_unhealthy_container_fails_once_the_monitor_window_ends() {
        let unhealthy = running(HealthObservation::Unhealthy);
        assert_eq!(
            ready(&unhealthy, Duration::from_secs(1), Some(DEADLINE)),
            HealthPoll::PendingUntil(MONITOR)
        );
        assert!(matches!(
            ready(&unhealthy, MONITOR, Some(DEADLINE)),
            HealthPoll::Failed(HealthFailure::Runtime { .. })
        ));
    }

    #[test]
    fn a_starting_container_times_out_at_its_health_deadline() {
        let starting = running(HealthObservation::Starting);
        assert_eq!(
            ready(&starting, Duration::from_secs(59), Some(DEADLINE)),
            HealthPoll::PendingUntil(DEADLINE)
        );
        assert!(matches!(
            ready(&starting, DEADLINE, Some(DEADLINE)),
            HealthPoll::Failed(HealthFailure::TimedOut { .. })
        ));
    }

    #[test]
    fn a_container_without_a_healthcheck_is_ready_once_it_outlives_the_monitor_window() {
        let unchecked = running(HealthObservation::NotConfigured);
        assert_eq!(
            ready(&unchecked, Duration::from_secs(4), None),
            HealthPoll::PendingUntil(MONITOR)
        );
        assert_eq!(ready(&unchecked, MONITOR, None), HealthPoll::Complete);
    }

    #[test]
    fn a_dependency_without_a_healthcheck_is_not_healthy() {
        assert!(matches!(
            classify_health(
                &running(HealthObservation::NotConfigured),
                None,
                MONITOR,
                MONITOR,
                None,
                HealthExpectation::Healthy,
            ),
            HealthPoll::Failed(HealthFailure::Runtime { .. })
        ));
    }

    #[test]
    fn the_spec_healthcheck_sets_the_deadline_without_a_start_clock() {
        let healthcheck = HealthcheckSpec::Http(crate::HttpHealthcheck {
            path: "/health".into(),
            port: std::num::NonZeroU16::new(8080).unwrap(),
            timeout_seconds: 300,
        });
        assert_eq!(healthcheck.deadline(), Some(Duration::from_secs(300)));
        assert_eq!(HealthcheckSpec::Disabled.deadline(), None);
    }
}
