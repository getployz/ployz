//! The daemon's memory of which Containers' checks passed since their current start.

use std::collections::BTreeMap;

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

    /// Health of a Running Container once its checks since this start are applied:
    /// `Starting` or `Unhealthy` after a pass since this start reports `Failing`.
    ///
    /// A start from before this daemon started counts as passed.
    pub(super) fn settle(
        &mut self,
        container_id: &ContainerId,
        started_at: Option<DateTime<Utc>>,
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
                Some(_) | None => started_at < self.daemon_started_at,
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
        health
    }

    pub(super) fn forget(&mut self, container_id: &ContainerId) {
        self.starts.remove(container_id);
    }
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

    #[test]
    fn a_docker_check_that_passed_once_reports_failing_afterwards() {
        let mut records = records();
        let id = container('a');
        let started = Some(at(0));
        let mut settle = |health| records.settle(&id, started, health);
        assert_eq!(
            settle(HealthObservation::Starting),
            HealthObservation::Starting
        );
        assert_eq!(
            settle(HealthObservation::Unhealthy),
            HealthObservation::Unhealthy
        );
        assert_eq!(
            settle(HealthObservation::Healthy),
            HealthObservation::Healthy
        );
        assert_eq!(
            settle(HealthObservation::Unhealthy),
            HealthObservation::Failing
        );
        assert_eq!(
            settle(HealthObservation::Healthy),
            HealthObservation::Healthy
        );
        assert_eq!(
            settle(HealthObservation::Unhealthy),
            HealthObservation::Failing
        );
    }

    #[test]
    fn an_http_probe_failure_before_the_first_pass_is_still_starting() {
        let mut records = records();
        let id = container('a');
        let started = Some(at(0));
        let mut settle = |health| records.settle(&id, started, health);
        assert_eq!(
            settle(HealthObservation::Starting),
            HealthObservation::Starting
        );
        assert_eq!(
            settle(HealthObservation::Healthy),
            HealthObservation::Healthy
        );
        assert_eq!(
            settle(HealthObservation::Starting),
            HealthObservation::Failing
        );
    }

    #[test]
    fn a_restart_under_the_same_id_begins_unpassed() {
        for never_passed in [HealthObservation::Unhealthy, HealthObservation::Starting] {
            let mut records = records();
            let id = container('a');
            records.settle(&id, Some(at(0)), HealthObservation::Healthy);
            assert_eq!(
                records.settle(&id, Some(at(0)), never_passed.clone()),
                HealthObservation::Failing
            );
            assert_eq!(
                records.settle(&id, Some(at(100)), never_passed.clone()),
                never_passed
            );
        }
    }

    #[test]
    fn a_start_from_before_the_daemon_counts_as_passed() {
        for health in [HealthObservation::Unhealthy, HealthObservation::Starting] {
            assert_eq!(
                records().settle(&container('a'), Some(at(-120)), health),
                HealthObservation::Failing
            );
        }
    }

    #[test]
    fn a_start_after_the_daemon_is_unpassed_until_it_passes() {
        for health in [HealthObservation::Unhealthy, HealthObservation::Starting] {
            assert_eq!(
                records().settle(&container('a'), Some(at(0)), health.clone()),
                health
            );
        }
    }

    #[test]
    fn without_a_start_nothing_counts_as_passed() {
        let mut records = records();
        let id = container('a');
        records.settle(&id, None, HealthObservation::Healthy);
        assert_eq!(
            records.settle(&id, None, HealthObservation::Unhealthy),
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
            assert_eq!(records.settle(&id, Some(at(0)), health.clone()), health);
        }
        assert!(records.starts.is_empty());
    }

    #[test]
    fn forgetting_a_container_drops_its_pass() {
        let mut records = records();
        let id = container('a');
        records.settle(&id, Some(at(0)), HealthObservation::Healthy);
        records.forget(&id);
        assert_eq!(
            records.settle(&id, Some(at(0)), HealthObservation::Unhealthy),
            HealthObservation::Unhealthy
        );
    }
}
