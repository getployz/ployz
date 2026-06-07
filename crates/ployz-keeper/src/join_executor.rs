//! Direct keeper join execution.

use ployz_core::ops::FailureMessage;

use crate::executor::{
    KeeperPlanExecution, KeeperPlanTerminal, KeeperStepEffects, KeeperStepEvent,
    KeeperStepRecorder, execute_keeper_plan, execute_labeled_action,
};
use crate::steps::{
    JoinToken, KeeperJoinTarget, KeeperStepFailureReason, KeeperStepLabel,
    keeper_join_install_plan, keeper_join_material_plan,
};

pub trait KeeperJoinRedeemer {
    fn redeem_join_token(&mut self, token: &JoinToken) -> Result<KeeperJoinTarget, FailureMessage>;
}

pub trait KeeperJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage>;
}

#[must_use]
pub fn execute_keeper_join(
    token: &JoinToken,
    redeemer: &mut impl KeeperJoinRedeemer,
    token_consumer: &mut impl KeeperJoinTokenConsumer,
    effects: &mut impl KeeperStepEffects,
    recorder: &mut impl KeeperStepRecorder,
) -> KeeperPlanExecution {
    let mut events = Vec::new();
    let target = match redeem_join_token(token, redeemer, recorder, &mut events) {
        Ok(target) => target,
        Err(execution) => return *execution,
    };

    let mut material_execution =
        execute_keeper_plan(&keeper_join_material_plan(&target), effects, recorder);
    let material_terminal = material_execution.terminal.clone();
    events.append(&mut material_execution.events);
    if material_terminal != KeeperPlanTerminal::Completed {
        return KeeperPlanExecution {
            events,
            terminal: material_terminal,
        };
    }

    let mut plan_execution =
        execute_keeper_plan(&keeper_join_install_plan(target), effects, recorder);
    let plan_terminal = plan_execution.terminal.clone();
    events.append(&mut plan_execution.events);
    if plan_terminal != KeeperPlanTerminal::Completed {
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
) -> Result<KeeperJoinTarget, Box<KeeperPlanExecution>> {
    execute_labeled_action(
        events,
        recorder,
        KeeperStepLabel::RedeemJoinToken,
        KeeperStepFailureReason::JoinTokenRedeemFailed,
        || redeemer.redeem_join_token(token),
    )
}
