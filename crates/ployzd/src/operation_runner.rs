//! Owned operation runners.

use ployz_core::ids::OperationId;
use ployz_core::ops::OperationOwnerLease;
use ployz_nats::operations::OperationStatusStoreError;
use std::future::Future;
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::deploy_worker::{
    ActiveServiceCommitter, DeployExecutionCommand, DeployExecutionError, DeployExecutionOutcome,
    DeployExecutionPorts, DeployHealthChecker, DeployOperationRecorder, DeployWorker,
    NodeContainerRuntime,
};
use crate::operation_lease::{OperationLeasePolicy, OperationLeasePolicyError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnedDeployRunner {
    lease_policy: OperationLeasePolicy,
}

impl OwnedDeployRunner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            lease_policy: OperationLeasePolicy::default_policy(),
        }
    }

    #[must_use]
    pub const fn with_lease_policy(lease_policy: OperationLeasePolicy) -> Self {
        Self { lease_policy }
    }

    pub fn try_with_renew_every(renew_every: Duration) -> Result<Self, OperationLeasePolicyError> {
        Ok(Self::with_lease_policy(OperationLeasePolicy::try_new(
            OperationLeasePolicy::default_policy().duration_seconds(),
            renew_every,
        )?))
    }

    pub async fn run<R, N, H, A, L>(
        &self,
        command: DeployExecutionCommand,
        ports: DeployExecutionPorts<'_, R, N, H, A>,
        lease_renewer: L,
    ) -> Result<DeployExecutionOutcome, DeployExecutionError>
    where
        R: DeployOperationRecorder,
        N: NodeContainerRuntime,
        H: DeployHealthChecker,
        A: ActiveServiceCommitter,
        L: OperationLeaseRenewer + Send + 'static,
    {
        let lease_task = AdvisoryLeaseTask::start(
            command.operation_id().clone(),
            self.lease_policy,
            lease_renewer,
        );
        let outcome = DeployWorker.execute(command, ports).await;
        lease_task.stop().await;
        outcome
    }
}

impl Default for OwnedDeployRunner {
    fn default() -> Self {
        Self::new()
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
        let _ = lease_renewer.renew_operation_lease(&operation_id).await;
        tokio::time::sleep(lease_policy.renew_every()).await;
    }
}
