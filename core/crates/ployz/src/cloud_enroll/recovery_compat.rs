//! Whether the daemon can found for Cloud's recovery flow. Every other site takes
//! this answer.

// TODO: delete the module once every shipped daemon has both RPCs.

use ployz_core::{
    DescribeContractRequest, ENROLL_FOR_RECOVERY_CAPABILITY, WRAP_SEALING_KEY_CAPABILITY, op,
};

use crate::connect::{Client, ConnectError};

/// Whether the daemon advertises `EnrollForRecovery` and `WrapSealingKey`.
///
/// # Errors
///
/// The daemon could not be reached.
pub(crate) async fn supported(client: &mut Client) -> Result<bool, ConnectError> {
    let contract = client
        .call_repeatable::<op::DescribeContract>(DescribeContractRequest {}, None)
        .await?;
    Ok(contract.supports(ENROLL_FOR_RECOVERY_CAPABILITY)
        && contract.supports(WRAP_SEALING_KEY_CAPABILITY))
}
