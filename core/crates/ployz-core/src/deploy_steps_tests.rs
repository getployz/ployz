use super::{DeploymentStep, StepName, Steps};
use crate::deploy_progress::{RowPhase, RowState, ServerProgress, StoppedBy};
use crate::{MachineId, ServiceName};

fn service() -> ServiceName {
    "api".parse().unwrap()
}

fn row(machine: u8, state: RowState) -> ServerProgress {
    ServerProgress {
        service: service(),
        machine: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
        server: format!("m{machine}"),
        state,
        stopped: false,
    }
}

fn running(phase: RowPhase) -> RowState {
    RowState::Running {
        phase,
        serving: false,
    }
}

fn step(name: StepName, started_at: i64, ended_at: Option<i64>) -> DeploymentStep {
    DeploymentStep {
        name,
        service: Some(service()),
        started_at,
        ended_at,
    }
}

#[test]
fn a_step_runs_from_its_first_row_in_to_its_last_row_out() {
    let mut steps = Steps::default();
    steps.rows(&[row(1, RowState::Pending), row(2, RowState::Pending)], 100);
    steps.rows(&[row(1, running(RowPhase::WaitingForHealth))], 200);
    steps.rows(&[row(2, running(RowPhase::WaitingForHealth))], 300);
    steps.rows(&[row(1, RowState::Completed)], 400);
    steps.rows(&[row(2, RowState::Completed)], 500);
    assert_eq!(
        steps.take_changed(),
        vec![
            step(StepName::Starting, 100, Some(300)),
            step(StepName::HealthCheck, 200, Some(500)),
        ]
    );
}

#[test]
fn a_failed_row_leaves_its_step_open() {
    let mut steps = Steps::default();
    steps.rows(&[row(1, running(RowPhase::CreatingContainer))], 100);
    steps.rows(&[row(1, running(RowPhase::WaitingForHealth))], 200);
    steps.rows(
        &[row(
            1,
            RowState::Failed {
                reason: "unhealthy".into(),
                cause: Vec::new(),
                step: None,
                log: None,
                unanswered: false,
                out_of_memory: None,
                stopped_by: StoppedBy::NOTHING,
            },
        )],
        300,
    );
    assert_eq!(
        steps.take_changed(),
        vec![
            step(StepName::Starting, 100, Some(200)),
            step(StepName::HealthCheck, 200, None),
        ]
    );
}

#[test]
fn a_step_that_failed_while_the_deploy_went_on_stays_open() {
    let mut steps = Steps::default();
    steps.hold(StepName::StoppingOld, Some(&service()));
    steps.rows(&[row(1, running(RowPhase::WaitingForSwitch))], 100);
    steps.rows(&[row(1, running(RowPhase::StoppingContainer))], 200);
    steps.hold(StepName::StoppingOld, Some(&service()));
    steps.rows(&[row(1, RowState::Completed)], 300);
    assert_eq!(
        steps.take_changed(),
        vec![
            step(StepName::SwitchingTraffic, 100, Some(200)),
            step(StepName::StoppingOld, 200, None),
        ]
    );
}

#[test]
fn the_last_end_of_a_step_is_its_latest_services() {
    let mut steps = Steps::default();
    let web: ServiceName = "web".parse().unwrap();
    assert_eq!(steps.last_ended(StepName::Building), None);
    steps.start(StepName::Building, Some(&service()), 100);
    steps.start(StepName::Building, Some(&web), 100);
    steps.end(StepName::Building, Some(&web), 300);
    steps.end(StepName::Building, Some(&service()), 200);
    assert_eq!(steps.last_ended(StepName::Building), Some(300));
}

#[test]
fn stopping_old_runs_until_the_copies_retired_after_the_outcome_are_gone() {
    let mut steps = Steps::default();
    let web: ServiceName = "web".parse().unwrap();
    steps.rows(&[row(1, running(RowPhase::StoppingContainer))], 100);
    steps.drain(StepName::StoppingOld, Some(&service()));
    steps.start(StepName::StoppingOld, Some(&web), 100);
    steps.drain(StepName::StoppingOld, Some(&web));
    steps.rows(&[row(1, RowState::Completed)], 110);
    steps.hold(StepName::StoppingOld, Some(&web));
    steps.drained(400);
    let mut changed = steps.take_changed();
    changed.sort_by(|a, b| a.service.cmp(&b.service));
    assert_eq!(
        changed,
        vec![
            step(StepName::StoppingOld, 100, Some(400)),
            DeploymentStep {
                service: Some(web),
                ..step(StepName::StoppingOld, 100, None)
            },
        ]
    );
}
