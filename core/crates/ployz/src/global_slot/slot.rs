//! A Global's copies on one Machine and the refusal for an eligibility that can't be told.

use ployz_core::{MachineId, QualifiedService, RpcError, ServicePlacementUnknownReason};

use crate::deploy::CopyLifecycle;
use crate::deploy::cleanup::Copy;

/// `machine_id`'s copies of `service`, as every Machine lists it.
pub(crate) async fn copies_on<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
) -> Result<Vec<Copy>, RpcError> {
    Ok(client
        .service_containers(service)
        .await?
        .iter()
        .filter(|observation| observation.machine_id == *machine_id)
        .map(Copy::observed)
        .collect())
}

/// The refusal for a slot whose eligibility on the Server can't be told.
pub(crate) fn unknown_eligibility(reason: ServicePlacementUnknownReason) -> RpcError {
    let why = match reason {
        ServicePlacementUnknownReason::MissingStorageEvidence => {
            "its storage support was not observed"
        }
    };
    RpcError::conflict(format!("eligibility on the Server is unknown: {why}"))
}
