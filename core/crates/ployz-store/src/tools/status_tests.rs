//! The canvas word's cases, from the dashboard's node-status tests.

use super::*;
use Container::{Down, Running, Serving, Stopped};

const SEEN: Watch = Watch::Observed { whole: true };

fn facts(containers: Option<&[Container]>, watch: Watch) -> ServiceFacts<'_> {
    ServiceFacts {
        new: false,
        empty_source: false,
        watch,
        containers,
        desired_replicas: None,
        deploying: false,
    }
}

fn status(containers: Option<&[Container]>, watch: Watch) -> ServiceStatus {
    service_status(&facts(containers, watch))
}

#[test]
fn says_what_runs_now_in_one_word() {
    assert_eq!(status(Some(&[Serving]), SEEN), ServiceStatus::Online);
    assert_eq!(status(Some(&[Down, Down]), SEEN), ServiceStatus::Crashed);
    assert_eq!(status(Some(&[]), SEEN), ServiceStatus::NotRunning);
}

#[test]
fn running_without_a_passed_healthcheck_is_degraded() {
    assert_eq!(status(Some(&[Running]), SEEN), ServiceStatus::Degraded);
}

#[test]
fn clean_exits_are_not_running_and_any_failure_exit_is_a_crash() {
    assert_eq!(status(Some(&[Stopped]), SEEN), ServiceStatus::NotRunning);
    assert_eq!(status(Some(&[Stopped, Down]), SEEN), ServiceStatus::Crashed);
}

#[test]
fn no_container_is_not_running_only_when_every_server_reported() {
    assert_eq!(status(None, SEEN), ServiceStatus::NotRunning);
    assert_eq!(
        status(None, Watch::Observed { whole: false }),
        ServiceStatus::CantReachServers
    );
}

#[test]
fn never_guesses_without_current_evidence() {
    for watch in [Watch::Connecting, Watch::Unavailable, Watch::Unreachable] {
        assert_eq!(
            status(Some(&[Serving]), watch),
            ServiceStatus::CantReachServers
        );
    }
    for watch in [Watch::NoServers, Watch::NoConnection] {
        assert_eq!(status(None, watch), ServiceStatus::NeedsAServer);
    }
}

#[test]
fn a_new_or_empty_service_is_not_deployed_whatever_the_servers_say() {
    let new = ServiceFacts {
        new: true,
        ..facts(None, Watch::Unreachable)
    };
    assert_eq!(service_status(&new), ServiceStatus::NotDeployed);
    let empty = ServiceFacts {
        empty_source: true,
        ..facts(Some(&[Serving]), SEEN)
    };
    assert_eq!(service_status(&empty), ServiceStatus::NotDeployed);
    // A new Service a failed Deploy still started says what its containers do.
    let started = ServiceFacts {
        new: true,
        ..facts(Some(&[Down]), SEEN)
    };
    assert_eq!(service_status(&started), ServiceStatus::Crashed);
}

#[test]
fn fewer_serving_replicas_than_asked_is_degraded_unless_rolling_or_partial() {
    let one: &[Container] = &[Serving, Stopped];
    let with = |desired, deploying, watch| {
        service_status(&ServiceFacts {
            desired_replicas: Some(desired),
            deploying,
            ..facts(Some(one), watch)
        })
    };
    assert_eq!(with(2, false, SEEN), ServiceStatus::Degraded);
    assert_eq!(with(2, true, SEEN), ServiceStatus::Online);
    assert_eq!(with(1, false, SEEN), ServiceStatus::Online);
    assert_eq!(
        with(2, false, Watch::Observed { whole: false }),
        ServiceStatus::Online
    );
}
