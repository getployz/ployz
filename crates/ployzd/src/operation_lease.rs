//! Operation lease policy.

use ployz_core::ops::OperationLeaseDurationSeconds;
use std::time::Duration;

pub const DEFAULT_OPERATION_LEASE_SECONDS: u64 = 60;
pub const DEFAULT_OPERATION_LEASE_RENEW_SECONDS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationLeasePolicy {
    duration_seconds: OperationLeaseDurationSeconds,
    renew_every: Duration,
}

impl OperationLeasePolicy {
    pub fn try_new(
        duration_seconds: OperationLeaseDurationSeconds,
        renew_every: Duration,
    ) -> Result<Self, OperationLeasePolicyError> {
        if renew_every.is_zero() {
            return Err(OperationLeasePolicyError::ZeroRenewalInterval);
        }

        let duration = Duration::from_secs(duration_seconds.get());
        if renew_every >= duration {
            return Err(OperationLeasePolicyError::RenewalNotBeforeExpiry {
                duration,
                renew_every,
            });
        }

        Ok(Self {
            duration_seconds,
            renew_every,
        })
    }

    #[must_use]
    pub fn default_policy() -> Self {
        Self::try_new(default_lease_seconds(), default_renew_every())
            .expect("default operation lease policy is valid")
    }

    #[must_use]
    pub const fn duration_seconds(self) -> OperationLeaseDurationSeconds {
        self.duration_seconds
    }

    #[must_use]
    pub const fn renew_every(self) -> Duration {
        self.renew_every
    }
}

impl Default for OperationLeasePolicy {
    fn default() -> Self {
        Self::default_policy()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationLeasePolicyError {
    ZeroRenewalInterval,
    RenewalNotBeforeExpiry {
        duration: Duration,
        renew_every: Duration,
    },
}

fn default_lease_seconds() -> OperationLeaseDurationSeconds {
    OperationLeaseDurationSeconds::try_new(DEFAULT_OPERATION_LEASE_SECONDS)
        .expect("default operation lease duration is valid")
}

const fn default_renew_every() -> Duration {
    Duration::from_secs(DEFAULT_OPERATION_LEASE_RENEW_SECONDS)
}
