//! Operation lease policy.

use ployz_core::ids::OperationId;
use ployz_core::ops::OperationLeaseDurationSeconds;
use ployz_core::ops::OperationOwnerLease;
use ployz_nats::operations::OperationStatusStoreError;
use std::future::Future;
use std::time::Duration;
use tokio::task::JoinHandle;

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

pub trait OperationLeaseRenewer {
    fn renew_operation_lease(
        &mut self,
        operation_id: &OperationId,
    ) -> impl Future<Output = Result<Option<OperationOwnerLease>, OperationStatusStoreError>> + Send;
}

impl OperationLeaseRenewer for crate::controllers::OperationControllers {
    async fn renew_operation_lease(
        &mut self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationOwnerLease>, OperationStatusStoreError> {
        crate::controllers::OperationControllers::renew_owner_lease(self, operation_id).await
    }
}

pub async fn with_advisory_operation_lease<T, F, L>(
    operation_id: OperationId,
    lease_policy: OperationLeasePolicy,
    lease_renewer: L,
    work: F,
) -> T
where
    F: Future<Output = T>,
    L: OperationLeaseRenewer + Send + 'static,
{
    let lease_task = AdvisoryLeaseTask::start(operation_id, lease_policy, lease_renewer);
    let outcome = work.await;
    lease_task.stop().await;
    outcome
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

struct AdvisoryLeaseTask {
    handle: JoinHandle<()>,
}

impl AdvisoryLeaseTask {
    fn start<L>(
        operation_id: OperationId,
        lease_policy: OperationLeasePolicy,
        lease_renewer: L,
    ) -> Self
    where
        L: OperationLeaseRenewer + Send + 'static,
    {
        let renewals = renew_lease_until_stopped(operation_id, lease_policy, lease_renewer);
        let handle = tokio::spawn(renewals);
        Self { handle }
    }

    async fn stop(mut self) {
        self.handle.abort();
        let _ = (&mut self.handle).await;
    }
}

impl Drop for AdvisoryLeaseTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn renew_lease_until_stopped<L>(
    operation_id: OperationId,
    lease_policy: OperationLeasePolicy,
    mut lease_renewer: L,
) where
    L: OperationLeaseRenewer,
{
    loop {
        tokio::time::sleep(lease_policy.renew_every()).await;
        let _ = lease_renewer.renew_operation_lease(&operation_id).await;
    }
}
