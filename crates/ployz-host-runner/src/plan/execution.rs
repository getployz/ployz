//! Ordered, bounded Host Runner plan execution and typed failures.

use ployz_core::ops::FailureMessage;

use super::steps::{
    HostRunnerStep, HostRunnerStepEffectError, HostRunnerStepFailure, HostRunnerStepFailureReason,
    HostRunnerStepLabel, HostRunnerStepPlan,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerPlanExecution {
    pub events: Vec<HostRunnerStepEvent>,
    pub terminal: HostRunnerPlanTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerPlanTerminal {
    Completed,
    Failed(Box<HostRunnerPlanFailure>),
}

impl HostRunnerPlanTerminal {
    #[must_use]
    pub fn failure(&self) -> Option<&HostRunnerPlanFailure> {
        match self {
            Self::Completed => None,
            Self::Failed(failure) => Some(failure),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerPlanFailure {
    Step(HostRunnerStepFailure),
    Record(HostRunnerRecordFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerStepEvent {
    Started { step: HostRunnerStepLabel },
    Succeeded { step: HostRunnerStepLabel },
    Failed(HostRunnerStepFailure),
}

pub trait HostRunnerStepEffects {
    fn apply_step(&mut self, step: &HostRunnerStep) -> Result<(), HostRunnerStepEffectError>;
}

pub trait HostRunnerStepRecorder {
    fn record_step_event(&mut self, event: &HostRunnerStepEvent) -> Result<(), FailureMessage>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerRecordFailure {
    pub event: HostRunnerStepEvent,
    pub message: FailureMessage,
}

pub(crate) fn execute_labeled_action<T>(
    events: &mut Vec<HostRunnerStepEvent>,
    recorder: &mut impl HostRunnerStepRecorder,
    label: HostRunnerStepLabel,
    reason: HostRunnerStepFailureReason,
    action: impl FnOnce() -> Result<T, FailureMessage>,
) -> Result<T, Box<HostRunnerPlanExecution>> {
    let started = HostRunnerStepEvent::Started {
        step: label.clone(),
    };
    if let Err(message) = record_event(events, recorder, started.clone()) {
        return Err(Box::new(failed_recording(events.clone(), started, message)));
    }

    match action() {
        Ok(value) => {
            let succeeded = HostRunnerStepEvent::Succeeded { step: label };
            record_event(events, recorder, succeeded.clone()).map_err(|message| {
                Box::new(failed_recording(events.clone(), succeeded, message))
            })?;
            Ok(value)
        }
        Err(message) => {
            let failure = HostRunnerStepFailure {
                step: label,
                reason,
                message,
            };
            let failed = HostRunnerStepEvent::Failed(failure.clone());
            if let Err(message) = record_event(events, recorder, failed.clone()) {
                return Err(Box::new(failed_recording(events.clone(), failed, message)));
            }
            Err(Box::new(HostRunnerPlanExecution {
                events: events.clone(),
                terminal: HostRunnerPlanTerminal::Failed(Box::new(HostRunnerPlanFailure::Step(
                    failure,
                ))),
            }))
        }
    }
}

#[must_use]
pub fn execute_host_runner_plan(
    plan: &HostRunnerStepPlan,
    effects: &mut impl HostRunnerStepEffects,
    recorder: &mut impl HostRunnerStepRecorder,
) -> HostRunnerPlanExecution {
    let mut events = Vec::new();

    for step in plan.steps() {
        let label = HostRunnerStepLabel::from_step(step);
        let started = HostRunnerStepEvent::Started {
            step: label.clone(),
        };
        if let Err(message) = record_event(&mut events, recorder, started.clone()) {
            return failed_recording(events, started, message);
        }

        match effects.apply_step(step) {
            Ok(()) => {
                let succeeded = HostRunnerStepEvent::Succeeded { step: label };
                if let Err(message) = record_event(&mut events, recorder, succeeded.clone()) {
                    return failed_recording(events, succeeded, message);
                }
            }
            Err(error) => {
                let failure = HostRunnerStepFailure::from_effect_error(step, error);
                let failed = HostRunnerStepEvent::Failed(failure.clone());
                if let Err(message) = record_event(&mut events, recorder, failed.clone()) {
                    return failed_recording(events, failed, message);
                }
                return HostRunnerPlanExecution {
                    events,
                    terminal: HostRunnerPlanTerminal::Failed(Box::new(
                        HostRunnerPlanFailure::Step(failure),
                    )),
                };
            }
        }
    }

    HostRunnerPlanExecution {
        events,
        terminal: HostRunnerPlanTerminal::Completed,
    }
}

pub(crate) fn record_event(
    events: &mut Vec<HostRunnerStepEvent>,
    recorder: &mut impl HostRunnerStepRecorder,
    event: HostRunnerStepEvent,
) -> Result<(), FailureMessage> {
    recorder.record_step_event(&event)?;
    events.push(event);
    Ok(())
}

pub(crate) fn failed_recording(
    events: Vec<HostRunnerStepEvent>,
    event: HostRunnerStepEvent,
    message: FailureMessage,
) -> HostRunnerPlanExecution {
    HostRunnerPlanExecution {
        events,
        terminal: HostRunnerPlanTerminal::Failed(Box::new(HostRunnerPlanFailure::Record(
            HostRunnerRecordFailure { event, message },
        ))),
    }
}
