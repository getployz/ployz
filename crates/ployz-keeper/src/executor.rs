//! Direct keeper step execution.

use ployz_core::ops::FailureMessage;

use crate::steps::{KeeperStep, KeeperStepFailure, KeeperStepLabel, KeeperStepPlan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperPlanExecution {
    pub events: Vec<KeeperStepEvent>,
    pub terminal: KeeperPlanTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperPlanTerminal {
    Completed,
    Failed(KeeperPlanFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperPlanFailure {
    Step(KeeperStepFailure),
    Record(KeeperRecordFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStepEvent {
    Started { step: KeeperStepLabel },
    Succeeded { step: KeeperStepLabel },
    Failed(KeeperStepFailure),
}

pub trait KeeperStepEffects {
    fn apply_step(&mut self, step: &KeeperStep) -> Result<(), FailureMessage>;
}

pub trait KeeperStepRecorder {
    fn record_step_event(&mut self, event: &KeeperStepEvent) -> Result<(), FailureMessage>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperRecordFailure {
    pub event: KeeperStepEvent,
    pub message: FailureMessage,
}

#[must_use]
pub fn execute_keeper_plan(
    plan: &KeeperStepPlan,
    effects: &mut impl KeeperStepEffects,
    recorder: &mut impl KeeperStepRecorder,
) -> KeeperPlanExecution {
    let mut events = Vec::new();

    for step in plan.steps() {
        let label = KeeperStepLabel::from_step(step);
        let started = KeeperStepEvent::Started {
            step: label.clone(),
        };
        if let Err(message) = record_event(&mut events, recorder, started.clone()) {
            return failed_recording(events, started, message);
        }

        match effects.apply_step(step) {
            Ok(()) => {
                let succeeded = KeeperStepEvent::Succeeded { step: label };
                if let Err(message) = record_event(&mut events, recorder, succeeded.clone()) {
                    return failed_recording(events, succeeded, message);
                }
            }
            Err(message) => {
                let failure = KeeperStepFailure::from_step(step, message);
                let failed = KeeperStepEvent::Failed(failure.clone());
                if let Err(message) = record_event(&mut events, recorder, failed.clone()) {
                    return failed_recording(events, failed, message);
                }
                return KeeperPlanExecution {
                    events,
                    terminal: KeeperPlanTerminal::Failed(KeeperPlanFailure::Step(failure)),
                };
            }
        }
    }

    KeeperPlanExecution {
        events,
        terminal: KeeperPlanTerminal::Completed,
    }
}

fn record_event(
    events: &mut Vec<KeeperStepEvent>,
    recorder: &mut impl KeeperStepRecorder,
    event: KeeperStepEvent,
) -> Result<(), FailureMessage> {
    recorder.record_step_event(&event)?;
    events.push(event);
    Ok(())
}

fn failed_recording(
    events: Vec<KeeperStepEvent>,
    event: KeeperStepEvent,
    message: FailureMessage,
) -> KeeperPlanExecution {
    KeeperPlanExecution {
        events,
        terminal: KeeperPlanTerminal::Failed(KeeperPlanFailure::Record(KeeperRecordFailure {
            event,
            message,
        })),
    }
}
