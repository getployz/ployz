//! First-machine activation: the bounded redeem-wait / seed-write / report
//! workflow that turns a freshly bootstrapped machine into the cluster's
//! first active machine.

use crate::adapters::nats_authorization::write_machine_seed_file;
use ployz_core::ids::MachineId;
use ployz_core::machine::plan_first_machine_activation;
use ployz_core::state::ActiveMachineState;
use ployz_sdk_types::{
    InitFirstMachineActivateError, InitFirstMachineActivateRequest, InitFirstMachineActivated,
    MachineAddRequest, MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinReported, MachineJoinToken,
};

use super::OperationApiHandlers;
use super::machine_join::{machine_join_redeem, machine_join_report};
use super::submit::machine_add;

const FIRST_MACHINE_MATERIAL_WAIT_ATTEMPTS: u32 = 120;
const FIRST_MACHINE_MATERIAL_WAIT_DELAY: std::time::Duration =
    std::time::Duration::from_millis(250);

/// Thin trigger: plan the activation, short-circuit if the first machine is
/// already active, accept the machine-add, then run the join workflow.
pub async fn init_first_machine_activate(
    handlers: &OperationApiHandlers,
    request: InitFirstMachineActivateRequest,
) -> Result<InitFirstMachineActivated, InitFirstMachineActivateError> {
    let public_url_mode = request.public_url_mode;
    let plan = plan_first_machine_activation(&request.machine_id)
        .map_err(|_| InitFirstMachineActivateError::InvalidPlan)?;
    if let Some(active) = first_machine_active_machine(handlers, &request.machine_id).await? {
        set_public_url_mode_if_unconfigured(handlers.lease_intent(), public_url_mode).await?;
        return Ok(InitFirstMachineActivated {
            operation_id: active.activated_by,
            machine_id: active.machine_id,
        });
    }
    let accepted = machine_add(
        handlers,
        MachineAddRequest {
            operation_id: plan.operation_id,
            idempotency_key: plan.idempotency_key,
            machine_id: request.machine_id,
            name: plan.name,
            roles: request.roles,
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        },
    )
    .await
    .map_err(|failure| InitFirstMachineActivateError::MachineAdd { failure })?;
    let reported = redeem_seed_and_report(handlers, accepted.join_token).await?;
    set_public_url_mode(handlers, public_url_mode).await?;

    Ok(InitFirstMachineActivated {
        operation_id: reported.operation_id,
        machine_id: reported.machine_id,
    })
}

async fn set_public_url_mode_if_unconfigured(
    lease_intent: &crate::intent::lease_intent::LeaseIntentStore,
    mode: ployz_core::cert::PublicUrlMode,
) -> Result<(), InitFirstMachineActivateError> {
    lease_intent
        .set_mode_if_unconfigured(mode)
        .await
        .map_err(|error| InitFirstMachineActivateError::Unavailable {
            message: error.to_string(),
        })
}

async fn set_public_url_mode(
    handlers: &OperationApiHandlers,
    mode: ployz_core::cert::PublicUrlMode,
) -> Result<(), InitFirstMachineActivateError> {
    handlers
        .lease_intent()
        .set_mode(mode)
        .await
        .map_err(|error| InitFirstMachineActivateError::Unavailable {
            message: error.to_string(),
        })
}

/// The first-machine join workflow: redeem the join token (waiting boundedly
/// for the mint worker's material-ready), write the machine seed locally, and
/// report join completion.
async fn redeem_seed_and_report(
    handlers: &OperationApiHandlers,
    join_token: MachineJoinToken,
) -> Result<MachineJoinReported, InitFirstMachineActivateError> {
    // The first machine's Machine user is minted through the same worker-side
    // path as any machine-add; redeem waits boundedly for material-ready.
    let redeemed = redeem_when_material_ready(handlers, &join_token).await?;
    // The named writer of machine.seed is ployzd control, which runs on this
    // machine: a local 0600 file write, no RPC hop.
    write_machine_seed_file(
        handlers.machine_mint.machine_seed_file(),
        &redeemed.secret_delivery.nats_credentials,
    )
    .map_err(|error| InitFirstMachineActivateError::MachineSeedWrite {
        message: machine_seed_write_failure_message(&error),
    })?;
    machine_join_report(
        handlers,
        MachineJoinReportRequest {
            join_token,
            outcome: MachineJoinReportOutcome::Completed,
        },
    )
    .await
    .map_err(|failure| InitFirstMachineActivateError::JoinReport { failure })
}

fn machine_seed_write_failure_message(
    error: &crate::adapters::nats_authorization::MachineSeedWriteError,
) -> ployz_core::ops::FailureMessage {
    match ployz_core::ops::FailureMessage::try_new(error.to_string()) {
        Ok(message) => message,
        Err(_) => ployz_core::ops::FailureMessage::try_new("machine seed write failed")
            .expect("static failure message is non-empty"),
    }
}

/// Redeems the first-machine join token, retrying boundedly while the mint
/// worker has not reached `material-ready` yet.
async fn redeem_when_material_ready(
    handlers: &OperationApiHandlers,
    join_token: &MachineJoinToken,
) -> Result<MachineJoinRedeemed, InitFirstMachineActivateError> {
    let mut last_not_ready: Option<MachineJoinRedeemError> = None;
    for _ in 0..FIRST_MACHINE_MATERIAL_WAIT_ATTEMPTS {
        match machine_join_redeem(
            handlers,
            MachineJoinRedeemRequest {
                join_token: join_token.clone(),
            },
        )
        .await
        {
            Ok(redeemed) => return Ok(redeemed),
            Err(failure @ MachineJoinRedeemError::MaterialNotReady { .. }) => {
                last_not_ready = Some(failure);
                tokio::time::sleep(FIRST_MACHINE_MATERIAL_WAIT_DELAY).await;
            }
            Err(failure) => return Err(InitFirstMachineActivateError::JoinRedeem { failure }),
        }
    }

    Err(InitFirstMachineActivateError::JoinRedeem {
        failure: last_not_ready.unwrap_or(MachineJoinRedeemError::UnknownJoinToken),
    })
}

async fn first_machine_active_machine(
    handlers: &OperationApiHandlers,
    machine_id: &MachineId,
) -> Result<Option<ActiveMachineState>, InitFirstMachineActivateError> {
    handlers
        .machine_roster
        .active_machine(machine_id)
        .await
        .map_err(|error| InitFirstMachineActivateError::Unavailable {
            message: error.to_string(),
        })
}
