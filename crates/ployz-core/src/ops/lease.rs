//! Operation ownership leases.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU64;

use crate::ids::{OperationId, OperationOwnerId};
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationOwnerLease {
    pub operation_id: OperationId,
    pub owner_id: OperationOwnerId,
    pub expires_at: OperationLeaseExpiresAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct OperationLeaseDurationSeconds(NonZeroU64);

impl OperationLeaseDurationSeconds {
    pub fn try_new(seconds: u64) -> Result<Self, OperationLeaseDurationError> {
        let Some(value) = NonZeroU64::new(seconds) else {
            return Err(OperationLeaseDurationError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for OperationLeaseDurationSeconds {
    type Error = OperationLeaseDurationError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperationLeaseDurationSeconds> for u64 {
    fn from(value: OperationLeaseDurationSeconds) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationLeaseDurationError {
    Zero,
}

impl fmt::Display for OperationLeaseDurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("operation lease duration must be greater than zero"),
        }
    }
}

impl OperationOwnerLease {
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        owner_id: OperationOwnerId,
        expires_at: OperationLeaseExpiresAt,
    ) -> Self {
        Self {
            operation_id,
            owner_id,
            expires_at,
        }
    }

    #[must_use]
    pub fn is_expired_at(&self, now: OperationLeaseExpiresAt) -> bool {
        self.expires_at <= now
    }

    #[must_use]
    pub fn renew_until(&self, expires_at: OperationLeaseExpiresAt) -> Self {
        Self {
            operation_id: self.operation_id.clone(),
            owner_id: self.owner_id.clone(),
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationOwnershipStatus {
    Unclaimed,
    Owned { lease: OperationOwnerLease },
    Expired { lease: OperationOwnerLease },
}

impl OperationOwnershipStatus {
    #[must_use]
    pub fn from_lease_at(lease: OperationOwnerLease, now: OperationLeaseExpiresAt) -> Self {
        if lease.is_expired_at(now) {
            Self::Expired { lease }
        } else {
            Self::Owned { lease }
        }
    }
}

positive_u64_wire_newtype! {
    pub struct OperationLeaseExpiresAt;
    ts_brand: "Brand<string, \"OperationLeaseExpiresAt\">";
    accessor: unix_seconds;
    error: OperationLeaseExpiresAtError;
}

positive_u64_wire_error! {
    pub enum OperationLeaseExpiresAtError;
    noun: "operation lease expiry";
}
