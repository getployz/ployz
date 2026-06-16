//! First-node activation: the bounded redeem-wait / seed-write / report
//! workflow that turns a freshly bootstrapped machine into the cluster's
//! first active node.

use crate::nats_authorization::write_node_seed_file;
use ployz_core::ids::NodeId;
use ployz_core::machine::plan_first_node_activation;
use ployz_core::state::ActiveMachineState;
use ployz_sdk_types::{
    InitFirstNodeActivateError, InitFirstNodeActivateRequest, InitFirstNodeActivated,
    MachineAddRequest, MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinReported, MachineJoinToken,
    MachineQueryUnavailableSource,
};

use super::OperationApiHandlers;
use super::machine_join::{machine_join_redeem, machine_join_report};
use super::submit::machine_add;

const FIRST_NODE_MATERIAL_WAIT_ATTEMPTS: u32 = 120;
const FIRST_NODE_MATERIAL_WAIT_DELAY: std::time::Duration = std::time::Duration::from_millis(250);

/// Thin trigger: plan the activation, short-circuit if the first node is
/// already active, accept the machine-add, then run the join workflow.
pub async fn init_first_node_activate(
    handlers: &OperationApiHandlers,
    request: InitFirstNodeActivateRequest,
) -> Result<InitFirstNodeActivated, InitFirstNodeActivateError> {
    let plan = plan_first_node_activation(&request.node_id)
        .map_err(|_| InitFirstNodeActivateError::InvalidPlan)?;
    if let Some(active) = first_node_active_machine(handlers, &request.node_id).await? {
        return Ok(InitFirstNodeActivated {
            operation_id: active.activated_by,
            node_id: active.node_id,
        });
    }
    let accepted = machine_add(
        handlers,
        MachineAddRequest {
            operation_id: plan.operation_id,
            idempotency_key: plan.idempotency_key,
            node_id: request.node_id,
            name: plan.name,
            roles: request.roles,
        },
    )
    .await
    .map_err(|failure| InitFirstNodeActivateError::MachineAdd { failure })?;
    let reported = redeem_seed_and_report(handlers, accepted.join_token).await?;

    Ok(InitFirstNodeActivated {
        operation_id: reported.operation_id,
        node_id: reported.node_id,
    })
}

/// The first-node join workflow: redeem the join token (waiting boundedly
/// for the mint worker's material-ready), write the node seed locally, and
/// report join completion.
async fn redeem_seed_and_report(
    handlers: &OperationApiHandlers,
    join_token: MachineJoinToken,
) -> Result<MachineJoinReported, InitFirstNodeActivateError> {
    // The first node's Node user is minted through the same worker-side
    // path as any machine-add; redeem waits boundedly for material-ready.
    let redeemed = redeem_when_material_ready(handlers, &join_token).await?;
    // The named writer of node.seed is ployzd control, which runs on this
    // machine: a local 0600 file write, no RPC hop.
    write_node_seed_file(
        handlers.machine_mint.node_seed_file(),
        &redeemed.secret_delivery.nats_credentials,
    )
    .map_err(|error| InitFirstNodeActivateError::NodeSeedWrite {
        message: node_seed_write_failure_message(&error),
    })?;
    machine_join_report(
        handlers,
        MachineJoinReportRequest {
            join_token,
            outcome: MachineJoinReportOutcome::Completed,
        },
    )
    .await
    .map_err(|failure| InitFirstNodeActivateError::JoinReport { failure })
}

fn node_seed_write_failure_message(
    error: &crate::nats_authorization::NodeSeedWriteError,
) -> ployz_core::ops::FailureMessage {
    match ployz_core::ops::FailureMessage::try_new(error.to_string()) {
        Ok(message) => message,
        Err(_) => ployz_core::ops::FailureMessage::try_new("node seed write failed")
            .expect("static failure message is non-empty"),
    }
}

/// Redeems the first-node join token, retrying boundedly while the mint
/// worker has not reached `material-ready` yet.
async fn redeem_when_material_ready(
    handlers: &OperationApiHandlers,
    join_token: &MachineJoinToken,
) -> Result<MachineJoinRedeemed, InitFirstNodeActivateError> {
    let mut last_not_ready: Option<MachineJoinRedeemError> = None;
    for _ in 0..FIRST_NODE_MATERIAL_WAIT_ATTEMPTS {
        match machine_join_redeem(
            handlers.controllers(),
            MachineJoinRedeemRequest {
                join_token: join_token.clone(),
            },
        )
        .await
        {
            Ok(redeemed) => return Ok(redeemed),
            Err(failure @ MachineJoinRedeemError::MaterialNotReady { .. }) => {
                last_not_ready = Some(failure);
                tokio::time::sleep(FIRST_NODE_MATERIAL_WAIT_DELAY).await;
            }
            Err(failure) => return Err(InitFirstNodeActivateError::JoinRedeem { failure }),
        }
    }

    Err(InitFirstNodeActivateError::JoinRedeem {
        failure: last_not_ready.unwrap_or(MachineJoinRedeemError::UnknownJoinToken),
    })
}

async fn first_node_active_machine(
    handlers: &OperationApiHandlers,
    node_id: &NodeId,
) -> Result<Option<ActiveMachineState>, InitFirstNodeActivateError> {
    handlers
        .core_state
        .active_machine(node_id)
        .await
        .map_err(|_| InitFirstNodeActivateError::Unavailable {
            source: MachineQueryUnavailableSource::CoreState,
        })
}
