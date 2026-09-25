//! Bounded enrollment after the caller has durably saved its allocation.
pub mod local;

use crate::connect::{Client, ConnectError};
use ployz_core::{
    EnrollmentAssignment, EnrollmentSnapshot, ListMachinesRequest, Registered, RpcError,
    RpcErrorCode, op,
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
        .call_unretried::<op::Register>(request, None)
        .await
        .map_err(|error| {
            if let ConnectError::Remote(error) = error {
                error
            } else {
                let mut error = RpcError::from(error);
                error.message = format!(
                    "Register response lost; outcome may be uncertain: {}",
                    error.message
                );
                error
            }
        })
}
