use std::time::Duration;

use serde::{Deserialize, Serialize};

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

/// The longest the Machine waits on a Container started from `spec` before it
/// answers a Settled creation.
#[must_use]
pub fn settle_deadline(spec: &ResolvedServiceSpec) -> Duration {
    let deadline = match &spec.container.healthcheck {
        Some(healthcheck) => healthcheck.deadline().unwrap_or_default(),
        None => healthcheck_deadline(None),
    };
    deadline.max(health_monitor(spec))
}

/// Which run of a Container one inspect saw. Docker gives each start a new one,
/// including the restarts its restart policy makes between two looks.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerRun {
    pub restart_count: i64,
    pub started_at_unix_nanos: i64,
}

/// A wait on a Container started just now to be ready. It holds the run it first
/// saw: a Container restarted during the wait isn't ready, however it looks when
/// sampled.
#[derive(Clone, Debug)]
pub struct ReadyWait<'spec> {
    healthcheck: Option<&'spec HealthcheckSpec>,
    monitor: Duration,
    first_run: Option<ContainerRun>,
}

impl<'spec> ReadyWait<'spec> {
    /// `healthcheck` is the Container's spec healthcheck when the caller has it.
    /// Without one the deadline comes from the healthcheck the Machine observed.
    #[must_use]
    pub fn new(healthcheck: Option<&'spec HealthcheckSpec>, monitor: Duration) -> Self {
        Self {
            healthcheck,
            monitor,
            first_run: None,
        }
    }

    /// One look at the Container: the verdict so far, and the offset the wait
    /// gives up at.
    pub fn poll(
        &mut self,
        details: &ContainerDetails,
        elapsed: Duration,
    ) -> (HealthPoll, Duration) {
        let observed = &details.container;
        let restarted = match &details.run {
            Some(run) => self.first_run.get_or_insert_with(|| run.clone()) != run,
            None => false,
        };
        let runtime = if restarted {
            &ContainerRuntimeObservation::Restarting
        } else {
            &observed.runtime
        };
        let health_deadline = health_deadline_for(self.healthcheck, observed);
        let deadline = health_deadline.unwrap_or(self.monitor);
        let monitor_deadline = if matches!(
            self.healthcheck.or(observed.effective_healthcheck.as_ref()),
            Some(HealthcheckSpec::Http(_))
        ) {
            deadline
        } else {
            self.monitor
        };
        let poll = classify_health(
            runtime,
            details.last_check.as_ref(),
            elapsed,
            monitor_deadline,
            health_deadline,
            HealthExpectation::ContainerReady,
        );
        (poll, deadline)
    }
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
                timed_out(health_deadline, last_check)
            } else {
                HealthPoll::PendingUntil(health_deadline)
            }
        }
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        } if let Some(health_deadline) = health_deadline
            && elapsed >= health_deadline =>
        {
            timed_out(health_deadline, last_check)
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

fn timed_out(deadline: Duration, last_check: Option<&LastHealthCheck>) -> HealthPoll {
    HealthPoll::Failed(HealthFailure::TimedOut {
        deadline_millis: u64::try_from(deadline.as_millis()).ok(),
        last_check: last_check.cloned(),
    })
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

    fn http_check(timeout_seconds: u16) -> HealthcheckSpec {
        HealthcheckSpec::Http(crate::HttpHealthcheck {
            path: "/health".into(),
            port: std::num::NonZeroU16::new(8080).unwrap(),
            timeout_seconds,
        })
    }

    fn configured_check(deadline_millis: u64) -> HealthcheckSpec {
        HealthcheckSpec::Configured(crate::ConfiguredHealthcheck {
            test: crate::HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
            interval_millis: None,
            timeout_millis: None,
            start_period_millis: None,
            start_interval_millis: None,
            retries: None,
            deadline_millis: Some(deadline_millis),
        })
    }

    fn service_spec(
        healthcheck: Option<HealthcheckSpec>,
        monitor_millis: Option<u64>,
    ) -> ResolvedServiceSpec {
        serde_json::from_value(serde_json::json!({
            "service_id": crate::ServiceId::random(),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "example.test/api",
                "pull_policy": "missing",
                "healthcheck": healthcheck,
            },
            "update": { "order": "start_first", "monitor_millis": monitor_millis },
        }))
        .unwrap()
    }

    fn details(
        runtime: ContainerRuntimeObservation,
        effective_healthcheck: Option<HealthcheckSpec>,
        last_check: Option<LastHealthCheck>,
    ) -> ContainerDetails {
        let container = ContainerObservation::try_from(crate::ContainerObservationParts {
            container_id: crate::ContainerId::parse("a".repeat(64)).unwrap(),
            display_name: "api".into(),
            created_at_unix_nanos: 0,
            machine_id: crate::MachineId::random(),
            namespace: crate::Namespace::parse("app").unwrap(),
            kind: crate::ContainerKind::ServiceContainer,
            runtime,
            effective_healthcheck,
            resolved_spec: service_spec(None, None),
            address: None,
            labels: Default::default(),
        })
        .unwrap();
        ContainerDetails {
            container,
            environment: None,
            image_id: None,
            last_check,
            run: None,
        }
    }

    fn run(restart_count: i64, started_at_unix_nanos: i64) -> Option<ContainerRun> {
        Some(ContainerRun {
            restart_count,
            started_at_unix_nanos,
        })
    }

    #[test]
    fn a_container_restarted_between_two_running_looks_is_not_ready() {
        let mut wait = ReadyWait::new(None, MONITOR);
        let mut looked = details(running(HealthObservation::NotConfigured), None, None);
        looked.run = run(0, 1);
        assert_eq!(
            wait.poll(&looked, Duration::from_secs(1)).0,
            HealthPoll::PendingUntil(MONITOR)
        );
        looked.run = run(1, 1);
        assert_eq!(
            wait.poll(&looked, MONITOR).0,
            HealthPoll::Failed(HealthFailure::Runtime {
                observation: ContainerRuntimeObservation::Restarting,
                last_check: None,
            })
        );
    }

    #[test]
    fn a_container_started_again_between_two_looks_is_not_ready() {
        let check = configured_check(60_000);
        let mut wait = ReadyWait::new(Some(&check), MONITOR);
        let mut starting = details(
            running(HealthObservation::Starting),
            Some(check.clone()),
            None,
        );
        starting.run = run(0, 1);
        assert!(matches!(
            wait.poll(&starting, Duration::from_secs(1)).0,
            HealthPoll::PendingUntil(_)
        ));
        let mut healthy = details(
            running(HealthObservation::Healthy),
            Some(check.clone()),
            None,
        );
        healthy.run = run(0, 2);
        assert!(matches!(
            wait.poll(&healthy, Duration::from_secs(2)).0,
            HealthPoll::Failed(HealthFailure::Runtime { .. })
        ));
    }

    #[test]
    fn a_container_on_one_run_through_the_monitor_window_is_ready() {
        let mut wait = ReadyWait::new(None, MONITOR);
        let mut looked = details(running(HealthObservation::NotConfigured), None, None);
        looked.run = run(3, 1);
        wait.poll(&looked, Duration::from_secs(1));
        assert_eq!(wait.poll(&looked, MONITOR).0, HealthPoll::Complete);
    }

    #[test]
    fn an_http_check_never_passed_by_its_deadline_times_out_naming_the_deadline() {
        let check = http_check(30);
        let last_check = LastHealthCheck::HttpStatus { status: 404 };
        let overdue = details(
            running(HealthObservation::Unhealthy),
            Some(check.clone()),
            Some(last_check.clone()),
        );
        assert_eq!(
            ReadyWait::new(Some(&check), MONITOR).poll(&overdue, Duration::from_secs(10)),
            (
                HealthPoll::PendingUntil(Duration::from_secs(30)),
                Duration::from_secs(30)
            )
        );
        let (poll, _) =
            ReadyWait::new(Some(&check), MONITOR).poll(&overdue, Duration::from_secs(30));
        assert_eq!(
            poll,
            HealthPoll::Failed(HealthFailure::TimedOut {
                deadline_millis: Some(30_000),
                last_check: Some(last_check),
            })
        );
    }

    #[test]
    fn a_docker_check_reported_unhealthy_fails_once_the_monitor_window_ends() {
        let check = configured_check(60_000);
        let unhealthy = details(
            running(HealthObservation::Unhealthy),
            Some(check.clone()),
            None,
        );
        assert_eq!(
            ReadyWait::new(Some(&check), MONITOR).poll(&unhealthy, Duration::from_secs(1)),
            (HealthPoll::PendingUntil(MONITOR), DEADLINE)
        );
        assert!(matches!(
            ReadyWait::new(Some(&check), MONITOR)
                .poll(&unhealthy, MONITOR)
                .0,
            HealthPoll::Failed(HealthFailure::Runtime { .. })
        ));
    }

    #[test]
    fn without_a_healthcheck_the_wait_is_the_monitor_window() {
        let unchecked = details(running(HealthObservation::NotConfigured), None, None);
        assert_eq!(
            ReadyWait::new(None, MONITOR).poll(&unchecked, Duration::from_secs(1)),
            (HealthPoll::PendingUntil(MONITOR), MONITOR)
        );
        assert_eq!(
            ReadyWait::new(None, MONITOR).poll(&unchecked, MONITOR),
            (HealthPoll::Complete, MONITOR)
        );
    }

    #[test]
    fn an_image_check_without_timings_gets_the_default_deadline() {
        let starting = details(running(HealthObservation::Starting), None, None);
        assert_eq!(
            ReadyWait::new(None, MONITOR)
                .poll(&starting, Duration::ZERO)
                .1,
            healthcheck_deadline(None)
        );
    }

    #[test]
    fn settle_deadline_is_the_longer_of_the_health_deadline_and_the_monitor() {
        assert_eq!(
            settle_deadline(&service_spec(Some(http_check(300)), None)),
            Duration::from_secs(300)
        );
        assert_eq!(
            settle_deadline(&service_spec(Some(configured_check(2_000)), Some(10_000))),
            Duration::from_secs(10)
        );
        assert_eq!(
            settle_deadline(&service_spec(Some(HealthcheckSpec::Disabled), None)),
            DEFAULT_HEALTH_MONITOR
        );
        assert_eq!(
            settle_deadline(&service_spec(None, None)),
            healthcheck_deadline(None)
        );
    }
}
