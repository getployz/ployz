//! The refusal for a Global's eligibility on a Machine that can't be told.

use ployz_core::{RpcError, ServicePlacementUnknownReason};

/// The refusal for a slot whose eligibility on the Server can't be told.
pub(crate) fn unknown_eligibility(reason: ServicePlacementUnknownReason) -> RpcError {
    let why = match reason {
        ServicePlacementUnknownReason::MissingStorageEvidence => {
            "its storage support was not observed"
        }
    };
    RpcError::conflict(format!("eligibility on the Server is unknown: {why}"))
}
