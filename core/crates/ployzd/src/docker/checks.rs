//! The daemon's memory of which Containers' checks passed since their current start.

use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, Utc};
use ployz_core::{ContainerId, HealthObservation};

struct ChecksSinceStart {
    started_at: DateTime<Utc>,
    passed: bool,
}

pub(super) struct CheckRecords {
    daemon_started_at: DateTime<Utc>,
    starts: BTreeMap<ContainerId, ChecksSinceStart>,
}

impl CheckRecords {
    pub(super) fn new(daemon_started_at: DateTime<Utc>) -> Self {
        Self {
            daemon_started_at,
            starts: BTreeMap::new(),
        }
    }

    pub(super) fn settle(
        &mut self,
        container_id: &ContainerId,
        started_at: Option<DateTime<Utc>>,
        first_pass_deadline: Option<Duration>,
        now: DateTime<Utc>,
        health: HealthObservation,
    ) -> HealthObservation {
        if !matches!(
            health,
            HealthObservation::Starting | HealthObservation::Healthy | HealthObservation::Unhealthy
        ) {
            return health;
        }
        let passed_before =
            started_at.is_some_and(|started_at| match self.starts.get(container_id) {
                Some(record) if record.started_at == started_at => record.passed,
                Some(_) | None => {
                    started_at < self.daemon_started_at
                        && first_pass_overdue(started_at, first_pass_deadline, now)
                }
            });
        if let Some(started_at) = started_at {
            self.starts.insert(
                *container_id,
                ChecksSinceStart {
                    started_at,
                    passed: passed_before || health == HealthObservation::Healthy,
                },
            );
        }
        if passed_before && health != HealthObservation::Healthy {
            return HealthObservation::Failing;
        }
        if !passed_before
            && health == HealthObservation::Starting
            && started_at
                .is_some_and(|started_at| first_pass_overdue(started_at, first_pass_deadline, now))
        {
            return HealthObservation::Unhealthy;
        }
        health
    }

    pub(super) fn forget(&mut self, container_id: &ContainerId) {
        self.starts.remove(container_id);
    }
}

fn first_pass_overdue(
    started_at: DateTime<Utc>,
    first_pass_deadline: Option<Duration>,
    now: DateTime<Utc>,
) -> bool {
    first_pass_deadline
        .is_some_and(|deadline| (now - started_at).to_std().is_ok_and(|age| age > deadline))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(hex: char) -> ContainerId {
        ContainerId::parse(hex.to_string().repeat(64)).unwrap()
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_000_000 + seconds, 0).unwrap()
    }

    fn records() -> CheckRecords {
        CheckRecords::new(at(-60))
    }

    const DEADLINE: Option<Duration> = Some(Duration::from_secs(30));

    #[test]
    fn a_docker_check_that_passed_once_reports_failing_afterwards() {
        let mut records = records();
        let id = container('a');
        let started = Some(at(0));
        let mut settle =
            |second, health| records.settle(&id, started, DEADLINE, at(second), health);
        assert_eq!(
            settle(1, HealthObservation::Starting),
            HealthObservation::Starting
        );
        assert_eq!(
            settle(31, HealthObservation::Unhealthy),
            HealthObservation::Unhealthy
        );
        assert_eq!(
            settle(40, HealthObservation::Healthy),
            HealthObservation::Healthy
        );
        assert_eq!(
            settle(70, HealthObservation::Unhealthy),
            HealthObservation::Failing
        );
        assert_eq!(
            settle(80, HealthObservation::Healthy),
            HealthObservation::Healthy
        );
        assert_eq!(
            settle(90, HealthObservation::Unhealthy),
            HealthObservation::Failing
        );
    }

    #[test]
    fn an_http_probe_failure_before_the_first_pass_is_still_starting() {
        let mut records = records();
        let id = container('a');
        let started = Some(at(0));
        let mut settle =
            |second, health| records.settle(&id, started, DEADLINE, at(second), health);
        assert_eq!(
            settle(1, HealthObservation::Starting),
            HealthObservation::Starting
        );
        assert_eq!(
            settle(2, HealthObservation::Healthy),
            HealthObservation::Healthy
        );
        assert_eq!(
            settle(3, HealthObservation::Starting),
            HealthObservation::Failing
        );
    }

    #[test]
    fn a_restart_under_the_same_id_begins_unpassed() {
        for never_passed in [HealthObservation::Unhealthy, HealthObservation::Starting] {
            let mut records = records();
            let id = container('a');
            records.settle(
                &id,
                Some(at(0)),
                DEADLINE,
                at(1),
                HealthObservation::Healthy,
            );
            assert_eq!(
                records.settle(&id, Some(at(0)), DEADLINE, at(2), never_passed.clone()),
                HealthObservation::Failing
            );
            assert_eq!(
                records.settle(&id, Some(at(100)), DEADLINE, at(101), never_passed.clone()),
                never_passed
            );
        }
    }

    #[test]
    fn an_http_check_that_never_passed_turns_unhealthy_after_its_deadline() {
        let mut records = records();
        let id = container('a');
        let started = Some(at(0));
        let mut settle =
            |second, health| records.settle(&id, started, DEADLINE, at(second), health);
        assert_eq!(
            settle(1, HealthObservation::Starting),
            HealthObservation::Starting
        );
        assert_eq!(
            settle(30, HealthObservation::Starting),
            HealthObservation::Starting
        );
        assert_eq!(
            settle(31, HealthObservation::Starting),
            HealthObservation::Unhealthy
        );
        assert_eq!(
            settle(40, HealthObservation::Healthy),
            HealthObservation::Healthy
        );
        assert_eq!(
            settle(50, HealthObservation::Starting),
            HealthObservation::Failing
        );
    }

    #[test]
    fn a_start_from_before_the_daemon_counts_as_passed() {
        for health in [HealthObservation::Unhealthy, HealthObservation::Starting] {
            assert_eq!(
                records().settle(&container('a'), Some(at(-120)), DEADLINE, at(1), health),
                HealthObservation::Failing
            );
        }
    }

    #[test]
    fn a_start_from_before_the_daemon_inside_its_deadline_has_not_passed() {
        assert_eq!(
            records().settle(
                &container('a'),
                Some(at(-61)),
                DEADLINE,
                at(-50),
                HealthObservation::Starting
            ),
            HealthObservation::Starting
        );
    }

    #[test]
    fn a_start_after_the_daemon_first_seen_past_its_deadline_is_unhealthy() {
        for health in [HealthObservation::Unhealthy, HealthObservation::Starting] {
            assert_eq!(
                records().settle(&container('a'), Some(at(0)), DEADLINE, at(31), health),
                HealthObservation::Unhealthy
            );
        }
    }

    #[test]
    fn without_a_start_nothing_counts_as_passed() {
        let mut records = records();
        let id = container('a');
        records.settle(&id, None, DEADLINE, at(1), HealthObservation::Healthy);
        assert_eq!(
            records.settle(&id, None, DEADLINE, at(500), HealthObservation::Unhealthy),
            HealthObservation::Unhealthy
        );
    }

    #[test]
    fn health_without_a_check_passes_through_unrecorded() {
        let mut records = records();
        let id = container('a');
        for health in [
            HealthObservation::NotConfigured,
            HealthObservation::Unrecognized("degraded".into()),
        ] {
            assert_eq!(
                records.settle(&id, Some(at(0)), DEADLINE, at(1), health.clone()),
                health
            );
        }
        assert!(records.starts.is_empty());
    }

    #[test]
    fn forgetting_a_container_drops_its_pass() {
        let mut records = records();
        let id = container('a');
        records.settle(
            &id,
            Some(at(0)),
            DEADLINE,
            at(1),
            HealthObservation::Healthy,
        );
        records.forget(&id);
        assert_eq!(
            records.settle(
                &id,
                Some(at(0)),
                DEADLINE,
                at(2),
                HealthObservation::Unhealthy
            ),
            HealthObservation::Unhealthy
        );
    }
}
