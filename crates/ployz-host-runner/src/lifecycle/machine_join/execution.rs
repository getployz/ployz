//! Machine Join keeps Redemption acceptance before local work and Report as later evidence.

use ployz_core::ids::{MachineId, OperationId};
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
    ) -> Result<RedeemedHostRunnerJoin, FailureMessage>;
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
pub struct RedeemedHostRunnerJoin {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub(crate) target: Result<HostRunnerJoinTarget, FailureMessage>,
    pub callback_result: Option<MachineJoinRedeemed>,
}

impl RedeemedHostRunnerJoin {
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        machine_id: MachineId,
        target: HostRunnerJoinTarget,
    ) -> Self {
        Self {
            operation_id,
            machine_id,
            target: Ok(target),
            callback_result: None,
        }
    }

    #[must_use]
    pub fn resolution_failed(
        operation_id: OperationId,
        machine_id: MachineId,
        failure: FailureMessage,
    ) -> Self {
        Self {
            operation_id,
            machine_id,
            target: Err(failure),
            callback_result: None,
        }
    }

    #[must_use]
    pub fn with_callback_result(mut self, callback_result: MachineJoinRedeemed) -> Self {
        self.callback_result = Some(callback_result);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerJoinExecution {
    pub execution: HostRunnerPlanExecution,
    pub redeemed: Option<RedeemedHostRunnerJoin>,
}

#[must_use]
pub fn execute_host_runner_join(
    token: &JoinToken,
    redeemer: &mut impl HostRunnerJoinRedeemer,
    reporter: &mut impl HostRunnerJoinReporter,
    token_consumer: &mut impl HostRunnerJoinTokenConsumer,
    effects: &mut impl HostRunnerStepEffects,
    recorder: &mut impl HostRunnerStepRecorder,
) -> HostRunnerPlanExecution {
    execute_host_runner_join_with_redeemed(
        token,
        redeemer,
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
            };
        }
    };
    let redeemed_evidence = redeemed.clone();
    let target = match resolve_join_target(&redeemed, recorder, &mut events) {
        Ok(target) => target,
        Err(execution) => {
            let terminal = execution.terminal.clone();
            events = execution.events;
            report_join_failure(&terminal, reporter, recorder, &mut events);
            return HostRunnerJoinExecution {
                execution: HostRunnerPlanExecution { events, terminal },
                redeemed: Some(redeemed_evidence),
            };
        }
    };

    let mut material_execution =
        execute_host_runner_plan(&host_runner_join_material_plan(&target), effects, recorder);
    let material_terminal = material_execution.terminal.clone();
    events.append(&mut material_execution.events);
    if material_terminal != HostRunnerPlanTerminal::Completed {
        report_join_failure(&material_terminal, reporter, recorder, &mut events);
        return HostRunnerJoinExecution {
            execution: HostRunnerPlanExecution {
                events,
                terminal: material_terminal,
            },
            redeemed: Some(redeemed_evidence),
        };
    }

    let mut plan_execution =
        execute_host_runner_plan(&host_runner_join_install_plan(target), effects, recorder);
    let plan_terminal = plan_execution.terminal.clone();
    events.append(&mut plan_execution.events);
    if plan_terminal != HostRunnerPlanTerminal::Completed {
        report_join_failure(&plan_terminal, reporter, recorder, &mut events);
        return HostRunnerJoinExecution {
            execution: HostRunnerPlanExecution {
                events,
                terminal: plan_terminal,
            },
            redeemed: Some(redeemed_evidence),
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
        report_join_failure(&terminal, reporter, recorder, &mut events);
        return HostRunnerJoinExecution {
            execution: HostRunnerPlanExecution { events, terminal },
            redeemed: Some(redeemed_evidence),
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
            redeemed: Some(redeemed_evidence),
        };
    }

    HostRunnerJoinExecution {
        execution: HostRunnerPlanExecution {
            events,
            terminal: HostRunnerPlanTerminal::Completed,
        },
        redeemed: Some(redeemed_evidence),
    }
}

fn resolve_join_target(
    redeemed: &RedeemedHostRunnerJoin,
    recorder: &mut impl HostRunnerStepRecorder,
    events: &mut Vec<HostRunnerStepEvent>,
) -> Result<HostRunnerJoinTarget, Box<HostRunnerPlanExecution>> {
    execute_labeled_action(
        events,
        recorder,
        HostRunnerStepLabel::ResolveJoinTarget,
        HostRunnerStepFailureReason::JoinTargetResolutionFailed,
        || redeemed.target.clone(),
    )
}

fn redeem_join_token(
    token: &JoinToken,
    redeemer: &mut impl HostRunnerJoinRedeemer,
    recorder: &mut impl HostRunnerStepRecorder,
    events: &mut Vec<HostRunnerStepEvent>,
) -> Result<RedeemedHostRunnerJoin, Box<HostRunnerPlanExecution>> {
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
    reporter: &mut impl HostRunnerJoinReporter,
    recorder: &mut impl HostRunnerStepRecorder,
    events: &mut Vec<HostRunnerStepEvent>,
) {
    let HostRunnerPlanTerminal::Failed(failure) = terminal else {
        return;
    };
    let message = failure_message(failure_summary(failure));

    let _ = execute_labeled_action(
        events,
        recorder,
        HostRunnerStepLabel::ReportJoinResult,
        HostRunnerStepFailureReason::JoinReportFailed,
        || {
            reporter.report_join_failed(MachineJoinReportFailure::BootstrapFailed {
                message: message.clone(),
            })
        },
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
