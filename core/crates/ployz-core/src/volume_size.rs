//! SetVolumeSize: raise the bound of a Provisioned Volume held on one Machine while its
//! Container keeps running. The bound is the ZFS `refquota`, so a grow is one property
//! write and no restart. A shrink is refused here: ZFS refuses a `refquota` below what the
//! dataset already holds, and the Volume's Container may be writing toward the old bound.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DockerVolumeName, ProvisionedVolumeMaximumBytes, RpcError, RpcErrorCode};

/// Raise the bound of this Machine's copy of Volume `name` to `maximum_bytes`, whether
/// it is the writer's root or a Mirror's slot. A copy already at `maximum_bytes` answers
/// that bound without a write, so a resend converges. A copy above it is refused as a
/// `Shrink`: a bound only grows.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeSizeRequest {
    pub name: DockerVolumeName,
    pub maximum_bytes: ProvisionedVolumeMaximumBytes,
}

/// The bound the Volume holds after the request, read back from ZFS.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeSizeReply {
    pub bound_bytes: ProvisionedVolumeMaximumBytes,
}

/// Why a Machine refused a `SetVolumeSize`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum SizeRefusal {
    /// No Volume of that name is held on this Machine.
    NotFound,
    /// The Volume exists but carries no bound: it is not Provisioned.
    NotProvisioned,
    /// A Volume Run holds the Volume read-only on this Machine.
    Switching,
    /// The Pool cannot grow by the difference.
    NoCapacity,
    /// The request is below the bound the Volume holds.
    Shrink,
}

/// `RpcError.details` of a refused `SetVolumeSize`: the reason and the bound it left.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeSizeRefused {
    pub reason: SizeRefusal,
    /// The bound the Volume holds now, when it is Provisioned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bound_bytes: Option<ProvisionedVolumeMaximumBytes>,
    /// What the Pool lacked, on a `NoCapacity` refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capacity: Option<crate::StorageCapacityError>,
}

impl SetVolumeSizeRefused {
    /// The refusal carried by an RPC error, if it is one.
    #[must_use]
    pub fn from_details(details: &serde_json::Value) -> Option<Self> {
        Self::deserialize(details).ok()
    }

    /// An RPC error that carries this refusal in `details`.
    #[must_use]
    pub fn rpc_error(self, message: impl Into<String>) -> RpcError {
        let code = match self.reason {
            SizeRefusal::NotFound => RpcErrorCode::NotFound,
            SizeRefusal::NoCapacity => RpcErrorCode::Unavailable,
            SizeRefusal::NotProvisioned | SizeRefusal::Switching | SizeRefusal::Shrink => {
                RpcErrorCode::Conflict
            }
        };
        RpcError {
            code,
            message: message.into(),
            details: serde_json::to_value(self).expect("a size refusal is JSON serializable"),
            cause: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use super::*;

    #[test]
    fn a_refusal_round_trips_through_rpc_error_details() {
        let refused = SetVolumeSizeRefused {
            reason: SizeRefusal::Shrink,
            bound_bytes: Some(ProvisionedVolumeMaximumBytes::new(
                NonZeroU64::new(2_000_000_000).unwrap(),
            )),
            capacity: None,
        };
        let error = refused.clone().rpc_error("too small");

        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(
            SetVolumeSizeRefused::from_details(&error.details),
            Some(refused)
        );
        assert_eq!(
            SetVolumeSizeRefused::from_details(&serde_json::Value::Null),
            None
        );
    }
}
