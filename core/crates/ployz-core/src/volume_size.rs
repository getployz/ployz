//! SetVolumeSize: change the bound of a Provisioned Volume held on one Machine while its
//! Container keeps running. The bound is the ZFS `refquota`, so a resize is one property
//! write and no restart. A shrink is opt-in and stops at a floor above what the Volume
//! already references: ZFS refuses a `refquota` below that, and the Volume's Container
//! may be writing toward the old bound.

use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DockerVolumeName, ProvisionedVolumeMaximumBytes, RpcError, RpcErrorCode};

/// Set the bound of this Machine's copy of Volume `name` to `maximum_bytes`, whether
/// it is the writer's root or a Mirror's slot. A copy already at `maximum_bytes` answers
/// that bound without a write, so a resend converges. A copy above it is refused as a
/// `Shrink` unless the request allows one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetVolumeSizeRequest {
    pub name: DockerVolumeName,
    pub maximum_bytes: ProvisionedVolumeMaximumBytes,
    /// The caller accepts a bound below the one held, down to [`shrink_floor`]. A daemon
    /// without `ployz.volume.size.shrink.v1` ignores it and refuses as `Shrink`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub allow_shrink: bool,
}

/// The smallest bound a shrink accepts for a Volume that references `referenced_bytes`:
/// what it holds plus a tenth of that, and at least 1 GB, so its Container keeps room to
/// write while the bound comes down.
#[must_use]
pub fn shrink_floor(referenced_bytes: u64) -> ProvisionedVolumeMaximumBytes {
    let floor = referenced_bytes.saturating_add((referenced_bytes / 10).max(crate::domain::GB));
    ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(floor).expect("a floor is at least 1 GB"))
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
    /// The request is below the smallest bound this Machine accepts: what the Volume
    /// references plus headroom.
    BelowFloor,
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
    /// The smallest bound accepted, on a `BelowFloor` refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub floor_bytes: Option<ProvisionedVolumeMaximumBytes>,
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
            SizeRefusal::NoCapacity
                if !matches!(
                    self.capacity,
                    None | Some(crate::StorageCapacityError::StorageCapacityUnknown { .. })
                ) =>
            {
                RpcErrorCode::Conflict
            }
            SizeRefusal::NoCapacity => RpcErrorCode::Unavailable,
            SizeRefusal::NotProvisioned
            | SizeRefusal::Switching
            | SizeRefusal::Shrink
            | SizeRefusal::BelowFloor => RpcErrorCode::Conflict,
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

    use super::*;

    #[test]
    fn a_refusal_round_trips_through_rpc_error_details() {
        let refused = SetVolumeSizeRefused {
            reason: SizeRefusal::Shrink,
            bound_bytes: Some(ProvisionedVolumeMaximumBytes::new(
                NonZeroU64::new(2_000_000_000).unwrap(),
            )),
            capacity: None,
            floor_bytes: None,
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

    #[test]
    fn a_below_floor_refusal_carries_the_floor() {
        let error = SetVolumeSizeRefused {
            reason: SizeRefusal::BelowFloor,
            bound_bytes: None,
            capacity: None,
            floor_bytes: Some(shrink_floor(20_000_000_000)),
        }
        .rpc_error("below the floor");

        assert_eq!(error.code, RpcErrorCode::Conflict);
        assert_eq!(
            error.details,
            serde_json::json!({"reason": "below_floor", "floor_bytes": 22_000_000_000u64})
        );
    }

    #[test]
    fn the_shrink_floor_keeps_a_tenth_and_at_least_a_gb_of_headroom() {
        assert_eq!(shrink_floor(0).get(), 1_000_000_000);
        assert_eq!(shrink_floor(20_000_000_000).get(), 22_000_000_000);
        assert_eq!(shrink_floor(u64::MAX - 1).get(), u64::MAX);
    }

    #[test]
    fn a_request_allows_a_shrink_only_when_it_says_so() {
        let request: SetVolumeSizeRequest =
            serde_json::from_value(serde_json::json!({"name": "data", "maximum_bytes": 5}))
                .unwrap();
        assert!(!request.allow_shrink);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"name": "data", "maximum_bytes": 5})
        );

        let request = SetVolumeSizeRequest {
            allow_shrink: true,
            ..request
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"name": "data", "maximum_bytes": 5, "allow_shrink": true})
        );
    }
}
