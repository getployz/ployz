use ployz_core::dataplane::{WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest};
use ployz_core::ids::OperationId;
use ployz_core::ops::{DeployEvidence, DeployTransition};
use ployz_core::state::{ActiveServiceCommit, ActiveServiceCommitRequest};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use std::future::Future;

use super::{
    ActiveServiceCommitError, DeployContainer, DeployHealthCheckError, DeployOperationRecordError,
    NodeContainerRuntimeError, NodeRunContainerOutcome, NodeRunContainerRequest,
};

pub trait DeployOperationRecorder {
    fn record_deploy_transition(
        &mut self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> impl Future<Output = Result<(), DeployOperationRecordError>> + Send;

    fn record_deploy_evidence(
        &mut self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> impl Future<Output = Result<(), DeployOperationRecordError>> + Send;
}

pub trait NodeContainerRuntime {
    fn run_container(
        &mut self,
        request: NodeRunContainerRequest,
    ) -> impl Future<Output = Result<NodeRunContainerOutcome, NodeContainerRuntimeError>> + Send;
}

pub trait WireGuardEbpfPreparer {
    fn prepare_wireguard_ebpf(
        &mut self,
        request: WireGuardEbpfPrepareRequest,
    ) -> impl Future<Output = Result<(), WireGuardEbpfPrepareError>> + Send;
}

pub trait DeployHealthChecker {
    fn wait_healthy(
        &mut self,
        containers: &[DeployContainer],
    ) -> impl Future<Output = Result<(), DeployHealthCheckError>> + Send;
}

pub trait ActiveServiceCommitter {
    fn commit_active_service(
        &mut self,
        request: ActiveServiceCommitRequest,
    ) -> impl Future<Output = Result<ActiveServiceCommit, ActiveServiceCommitError>> + Send;
}

impl DeployOperationRecorder for crate::controllers::OperationControllers {
    async fn record_deploy_transition(
        &mut self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<(), DeployOperationRecordError> {
        crate::controllers::OperationControllers::record_deploy_transition(
            self,
            operation_id,
            transition,
        )
        .await
        .map(|_| ())
        .map_err(DeployOperationRecordError::RecordTransition)
    }

    async fn record_deploy_evidence(
        &mut self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<(), DeployOperationRecordError> {
        crate::controllers::OperationControllers::record_deploy_evidence(
            self,
            operation_id,
            evidence,
        )
        .await
        .map(|_| ())
        .map_err(DeployOperationRecordError::RecordEvidence)
    }
}

impl ActiveServiceCommitter for AsyncNatsCoreStateStore {
    async fn commit_active_service(
        &mut self,
        request: ActiveServiceCommitRequest,
    ) -> Result<ActiveServiceCommit, ActiveServiceCommitError> {
        AsyncNatsCoreStateStore::commit_active_service(self, &request)
            .await
            .map_err(ActiveServiceCommitError::Store)
    }
}
