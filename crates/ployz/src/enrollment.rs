//! Bounded enrollment after the caller has durably saved its allocation.
pub mod local;

use crate::connect::Client;
use ployz_core::{
    CloudPairing, EnrollmentAssignment, EnrollmentSnapshot, JoinAccepted, JoinRequest,
    ListMachinesRequest, Registered, RpcError, RpcErrorCode, op,
};

/// Observe the Entry Machine's enrollment facts before taking an operator lock.
///
/// # Errors
/// Returns transport errors or missing enrollment facts.
pub async fn observe_enrollment(entry: &mut Client) -> Result<EnrollmentSnapshot, RpcError> {
    entry
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await?
        .enrollment
        .ok_or_else(|| RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Entry Machine returned no enrollment snapshot".into(),
            details: serde_json::Value::Null,
        })
}

/// Publish a durably saved assignment before bootstrapping the joining Machine.
///
/// # Errors
/// Returns identity conflicts and Entry Machine publication/transport errors.
pub async fn publish_enrollment(
    entry: &mut Client,
    assignment: &EnrollmentAssignment,
) -> Result<Registered, RpcError> {
    let snapshot = observe_enrollment(entry).await?;
    ployz_core::allocate_enrollment(
        &assignment.request,
        &snapshot,
        std::slice::from_ref(assignment),
    )
    .map_err(|error| RpcError {
        code: RpcErrorCode::Conflict,
        message: error.to_string(),
        details: serde_json::Value::Null,
    })?;
    let mut request = assignment.request.clone();
    request.assigned_subnet = Some(assignment.machine.subnet);
    request.runtime.clone_from(&assignment.machine.runtime);
    entry
        .call::<op::Register>(request, None)
        .await
        .map_err(Into::into)
}

/// Re-publish a saved allocation, then durably accept Join. A lost response can
/// be retried with the same allocation; saving it alone never skips either RPC.
///
/// # Errors
/// Returns publication errors before attempting Join, or Join validation/transport errors.
pub async fn join_enrollment(
    entry: &mut Client,
    joining: &mut Client,
    assignment: &EnrollmentAssignment,
    wireguard_mtu: Option<u32>,
    cloud_pairing: Option<CloudPairing>,
) -> Result<JoinAccepted, RpcError> {
    let local = joining
        .call::<op::Inspect>(ployz_core::InspectRequest::default(), None)
        .await?;
    if local.id != assignment.machine.id
        || local.public_key != assignment.machine.public_key
        || !matches!(
            local.phase,
            ployz_core::LocalMachinePhase::Uninitialized
                | ployz_core::LocalMachinePhase::Joining
                | ployz_core::LocalMachinePhase::Participating
        )
    {
        return Err(RpcError {
            code: RpcErrorCode::Conflict,
            message: "saved enrollment does not match the joining Machine identity".into(),
            details: serde_json::Value::Null,
        });
    }
    let registration = publish_enrollment(entry, assignment).await?;
    joining
        .call::<op::Join>(
            JoinRequest {
                registration,
                wireguard_mtu,
                cloud_pairing,
            },
            None,
        )
        .await
        .map_err(Into::into)
}
