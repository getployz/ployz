//! Machine Join keeps Redemption acceptance before local work and Report as later evidence.

use ployz_core::operation::FailureMessage;
use ployz_sdk_types::{MachineJoinRedeemed, MachineJoinReportFailure};

use crate::plan::execution::execute_labeled_action;
use crate::plan::{
    HostRunnerJoinTarget, HostRunnerStepFailureReason, HostRunnerStepLabel, JoinToken,
    host_runner_join_install_plan, host_runner_join_material_plan,
};
use crate::plan::{
    HostRunnerPlanExecution, HostRunnerPlanFailure, HostRunnerPlanTerminal, HostRunnerStepEffects,
    HostRunnerStepEvent, HostRunnerStepRecorder, execute_host_runner_plan,
};

pub trait HostRunnerJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<MachineJoinRedeemed, FailureMessage>;
}

pub trait HostRunnerJoinResolver {
    fn resolve_join_target(
        &mut self,
        redeemed: &MachineJoinRedeemed,
    ) -> Result<HostRunnerJoinTarget, JoinTargetResolutionFailure>;
}

pub trait HostRunnerJoinReporter {
    fn report_join_completed(&mut self) -> Result<(), FailureMessage>;

    fn report_join_failed(
        &mut self,
        failure: MachineJoinReportFailure,
    ) -> Result<(), FailureMessage>;
}

pub trait HostRunnerJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTargetResolutionFailure {
    ReleasePlatformMissing,
    ReleasePlatformUnsupported { platform: String },
    Other { message: FailureMessage },
}

impl JoinTargetResolutionFailure {
    fn message(&self) -> FailureMessage {
        match self {
            Self::ReleasePlatformMissing => {
                failure_message("release manifest is missing PLOYZ_RELEASE_PLATFORM")
            }
            Self::ReleasePlatformUnsupported { platform } => {
                failure_message(&format!("unsupported release platform {platform}"))
            }
            Self::Other { message } => message.clone(),
        }
    }

    fn report_failure(&self) -> MachineJoinReportFailure {
        match self {
            Self::ReleasePlatformMissing => MachineJoinReportFailure::ReleasePlatformMissing,
            Self::ReleasePlatformUnsupported { platform } => {
                MachineJoinReportFailure::ReleasePlatformUnsupported {
                    platform: platform.clone(),
                }
            }
            Self::Other { message } => MachineJoinReportFailure::BootstrapFailed {
                message: message.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerJoinExecution {
    pub execution: HostRunnerPlanExecution,
    pub redeemed: Option<MachineJoinRedeemed>,
    pub resolution_failure: Option<JoinTargetResolutionFailure>,
}

#[must_use]
pub fn execute_host_runner_join(
    token: &JoinToken,
    redeemer: &mut impl HostRunnerJoinRedeemer,
    resolver: &mut impl HostRunnerJoinResolver,
    reporter: &mut impl HostRunnerJoinReporter,
    token_consumer: &mut impl HostRunnerJoinTokenConsumer,
    effects: &mut impl HostRunnerStepEffects,
    recorder: &mut impl HostRunnerStepRecorder,
) -> HostRunnerPlanExecution {
    execute_host_runner_join_with_redeemed(
        token,
        redeemer,
        resolver,
        reporter,
        token_consumer,
        effects,
        recorder,
    )
    .execution
}

#[must_use]
pub fn execute_host_runner_join_with_redeemed(
    token: &JoinToken,
    redeemer: &mut impl HostRunnerJoinRedeemer,
    resolver: &mut impl HostRunnerJoinResolver,
    reporter: &mut impl HostRunnerJoinReporter,
    token_consumer: &mut impl HostRunnerJoinTokenConsumer,
    effects: &mut impl HostRunnerStepEffects,
    recorder: &mut impl HostRunnerStepRecorder,
) -> HostRunnerJoinExecution {
    let mut events = Vec::new();
    let redeemed = match redeem_join_token(token, redeemer, recorder, &mut events) {
        Ok(redeemed) => redeemed,
        Err(execution) => {
            return HostRunnerJoinExecution {
                execution: *execution,
                redeemed: None,
                resolution_failure: None,
            };
        }
    };
    let target = match resolve_join_target(&redeemed, resolver, recorder, &mut events) {
        Ok(target) => target,
        Err((execution, resolution_failure)) => {
            let terminal = execution.terminal.clone();
            events = execution.events;
            report_join_failure(
                &terminal,
                Some(&resolution_failure),
                reporter,
                recorder,
                &mut events,
            );
            return HostRunnerJoinExecution {
                execution: HostRunnerPlanExecution { events, terminal },
                redeemed: Some(redeemed),
                resolution_failure: Some(resolution_failure),
            };
        }
    };

    let mut material_execution =
        execute_host_runner_plan(&host_runner_join_material_plan(&target), effects, recorder);
    let material_terminal = material_execution.terminal.clone();
    events.append(&mut material_execution.events);
    if material_terminal != HostRunnerPlanTerminal::Completed {
        report_join_failure(&material_terminal, None, reporter, recorder, &mut events);
        return HostRunnerJoinExecution {
            execution: HostRunnerPlanExecution {
                events,
                terminal: material_terminal,
            },
            redeemed: Some(redeemed),
            resolution_failure: None,
        };
    }

    let mut plan_execution =
        execute_host_runner_plan(&host_runner_join_install_plan(target), effects, recorder);
    let plan_terminal = plan_execution.terminal.clone();
    events.append(&mut plan_execution.events);
    if plan_terminal != HostRunnerPlanTerminal::Completed {
        report_join_failure(&plan_terminal, None, reporter, recorder, &mut events);
        return HostRunnerJoinExecution {
            execution: HostRunnerPlanExecution {
                events,
                terminal: plan_terminal,
            },
            redeemed: Some(redeemed),
            resolution_failure: None,
        };
    }

    if let Err(execution) = execute_labeled_action(
        &mut events,
        recorder,
        HostRunnerStepLabel::ConsumeJoinTokenFile,
        HostRunnerStepFailureReason::JoinTokenConsumeFailed,
        || token_consumer.consume_join_token(),
    ) {
        let terminal = execution.terminal.clone();
        events = execution.events;
        report_join_failure(&terminal, None, reporter, recorder, &mut events);
        return HostRunnerJoinExecution {
            execution: HostRunnerPlanExecution { events, terminal },
            redeemed: Some(redeemed),
            resolution_failure: None,
        };
    }

    if let Err(execution) = execute_labeled_action(
        &mut events,
        recorder,
        HostRunnerStepLabel::ReportJoinResult,
        HostRunnerStepFailureReason::JoinReportFailed,
        || reporter.report_join_completed(),
    ) {
        return HostRunnerJoinExecution {
            execution: *execution,
            redeemed: Some(redeemed),
            resolution_failure: None,
        };
    }

    HostRunnerJoinExecution {
        execution: HostRunnerPlanExecution {
            events,
            terminal: HostRunnerPlanTerminal::Completed,
        },
        redeemed: Some(redeemed),
        resolution_failure: None,
    }
}

fn resolve_join_target(
    redeemed: &MachineJoinRedeemed,
    resolver: &mut impl HostRunnerJoinResolver,
    recorder: &mut impl HostRunnerStepRecorder,
    events: &mut Vec<HostRunnerStepEvent>,
) -> Result<HostRunnerJoinTarget, (Box<HostRunnerPlanExecution>, JoinTargetResolutionFailure)> {
    let mut resolution_failure = None;
    let result = execute_labeled_action(
        events,
        recorder,
        HostRunnerStepLabel::ResolveJoinTarget,
        HostRunnerStepFailureReason::JoinTargetResolutionFailed,
        || {
            resolver.resolve_join_target(redeemed).map_err(|failure| {
                let message = failure.message();
                resolution_failure = Some(failure);
                message
            })
        },
    );
    result.map_err(|execution| {
        (
            execution,
            resolution_failure.expect("failed resolution records its typed failure"),
        )
    })
}

fn redeem_join_token(
    token: &JoinToken,
    redeemer: &mut impl HostRunnerJoinRedeemer,
    recorder: &mut impl HostRunnerStepRecorder,
    events: &mut Vec<HostRunnerStepEvent>,
) -> Result<MachineJoinRedeemed, Box<HostRunnerPlanExecution>> {
    execute_labeled_action(
        events,
        recorder,
        HostRunnerStepLabel::RedeemJoinToken,
        HostRunnerStepFailureReason::JoinTokenRedeemFailed,
        || redeemer.redeem_join_token(token),
    )
}

fn report_join_failure(
    terminal: &HostRunnerPlanTerminal,
    resolution_failure: Option<&JoinTargetResolutionFailure>,
    reporter: &mut impl HostRunnerJoinReporter,
    recorder: &mut impl HostRunnerStepRecorder,
    events: &mut Vec<HostRunnerStepEvent>,
) {
    let HostRunnerPlanTerminal::Failed(failure) = terminal else {
        return;
    };
    let report_failure = resolution_failure.map_or_else(
        || MachineJoinReportFailure::BootstrapFailed {
            message: failure_message(failure_summary(failure)),
        },
        JoinTargetResolutionFailure::report_failure,
    );

    let _ = execute_labeled_action(
        events,
        recorder,
        HostRunnerStepLabel::ReportJoinResult,
        HostRunnerStepFailureReason::JoinReportFailed,
        || reporter.report_join_failed(report_failure.clone()),
    );
}

fn failure_summary(failure: &HostRunnerPlanFailure) -> &str {
    match failure {
        HostRunnerPlanFailure::Step(step) => step.message.as_str(),
        HostRunnerPlanFailure::Record(record) => record.message.as_str(),
    }
}

fn failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("Host Runner failure message is non-empty")
}
