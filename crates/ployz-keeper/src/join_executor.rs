//! Direct keeper join execution.

use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::FailureMessage;
use ployz_sdk_types::MachineJoinReportFailure;

use crate::executor::{
    KeeperPlanExecution, KeeperPlanFailure, KeeperPlanTerminal, KeeperStepEffects, KeeperStepEvent,
    KeeperStepRecorder, execute_keeper_plan, execute_labeled_action,
};
use crate::steps::{
    JoinToken, KeeperJoinTarget, KeeperStepFailureReason, KeeperStepLabel,
    keeper_join_install_plan, keeper_join_material_plan,
};

pub trait KeeperJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage>;
}

pub trait KeeperJoinReporter {
    fn report_join_completed(&mut self) -> Result<(), FailureMessage>;

    fn report_join_failed(
        &mut self,
        failure: MachineJoinReportFailure,
    ) -> Result<(), FailureMessage>;
}

pub trait KeeperJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedKeeperJoin {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target: KeeperJoinTarget,
}

impl RedeemedKeeperJoin {
    #[must_use]
    pub fn new(operation_id: OperationId, machine_id: MachineId, target: KeeperJoinTarget) -> Self {
        Self {
            operation_id,
            machine_id,
            target,
        }
    }
}

#[must_use]
pub fn execute_keeper_join(
    token: &JoinToken,
    redeemer: &mut impl KeeperJoinRedeemer,
    reporter: &mut impl KeeperJoinReporter,
    token_consumer: &mut impl KeeperJoinTokenConsumer,
    effects: &mut impl KeeperStepEffects,
    recorder: &mut impl KeeperStepRecorder,
) -> KeeperPlanExecution {
    let mut events = Vec::new();
    let redeemed = match redeem_join_token(token, redeemer, recorder, &mut events) {
        Ok(redeemed) => redeemed,
        Err(execution) => return *execution,
    };

    let mut material_execution = execute_keeper_plan(
        &keeper_join_material_plan(&redeemed.target),
        effects,
        recorder,
    );
    let material_terminal = material_execution.terminal.clone();
    events.append(&mut material_execution.events);
    if material_terminal != KeeperPlanTerminal::Completed {
        report_join_failure(&material_terminal, reporter, recorder, &mut events);
        return KeeperPlanExecution {
            events,
            terminal: material_terminal,
        };
    }

    let mut plan_execution = execute_keeper_plan(
        &keeper_join_install_plan(redeemed.target.clone()),
        effects,
        recorder,
    );
    let plan_terminal = plan_execution.terminal.clone();
    events.append(&mut plan_execution.events);
    if plan_terminal != KeeperPlanTerminal::Completed {
        report_join_failure(&plan_terminal, reporter, recorder, &mut events);
        return KeeperPlanExecution {
            events,
            terminal: plan_terminal,
        };
    }

    if let Err(execution) = execute_labeled_action(
        &mut events,
        recorder,
        KeeperStepLabel::ConsumeJoinTokenFile,
        KeeperStepFailureReason::JoinTokenConsumeFailed,
        || token_consumer.consume_join_token(),
    ) {
        let terminal = execution.terminal.clone();
        events = execution.events;
        report_join_failure(&terminal, reporter, recorder, &mut events);
        return KeeperPlanExecution { events, terminal };
    }

    if let Err(execution) = execute_labeled_action(
        &mut events,
        recorder,
        KeeperStepLabel::ReportJoinResult,
        KeeperStepFailureReason::JoinReportFailed,
        || reporter.report_join_completed(),
    ) {
        return *execution;
    }

    KeeperPlanExecution {
        events,
        terminal: KeeperPlanTerminal::Completed,
    }
}

fn redeem_join_token(
    token: &JoinToken,
    redeemer: &mut impl KeeperJoinRedeemer,
    recorder: &mut impl KeeperStepRecorder,
    events: &mut Vec<KeeperStepEvent>,
) -> Result<RedeemedKeeperJoin, Box<KeeperPlanExecution>> {
    execute_labeled_action(
        events,
        recorder,
        KeeperStepLabel::RedeemJoinToken,
        KeeperStepFailureReason::JoinTokenRedeemFailed,
        || redeemer.redeem_join_token(token),
    )
}

fn report_join_failure(
    terminal: &KeeperPlanTerminal,
    reporter: &mut impl KeeperJoinReporter,
    recorder: &mut impl KeeperStepRecorder,
    events: &mut Vec<KeeperStepEvent>,
) {
    let KeeperPlanTerminal::Failed(failure) = terminal else {
        return;
    };
    let message = failure_message(failure_summary(failure));

    let _ = execute_labeled_action(
        events,
        recorder,
        KeeperStepLabel::ReportJoinResult,
        KeeperStepFailureReason::JoinReportFailed,
        || {
            reporter.report_join_failed(MachineJoinReportFailure::BootstrapFailed {
                message: message.clone(),
            })
        },
    );
}

fn failure_summary(failure: &KeeperPlanFailure) -> &str {
    match failure {
        KeeperPlanFailure::Step(step) => step.message.as_str(),
        KeeperPlanFailure::Record(record) => record.message.as_str(),
    }
}

fn failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("keeper failure message is non-empty")
}
