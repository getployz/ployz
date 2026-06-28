use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareProviderReport, DataplanePrepareRequest,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::{DeployEvidence, DeployTransition};
use ployz_core::state::{
    ActiveRouteCommit, ActiveRouteCommitRequest, ActiveServiceCommit, ActiveServiceCommitRequest,
};
use ployz_nats::core_state::{ActiveRouteStoreError, AsyncNatsCoreStateStore};
use std::future::Future;

use crate::machine_runtime::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerStopRpcRequest, MachineEnsureEndpointNetworkRpcRequest,
    MachineRunContainerOutcome,
};

use super::{
    ActiveServiceCommitError, DeployContainer, DeployHealthCheckError, DeployOperationRecordError,
    MachineContainerRuntimeError,
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

pub trait MachineContainerRuntime {
    fn ensure_endpoint_network(
        &mut self,
        machine_id: &MachineId,
        request: MachineEnsureEndpointNetworkRpcRequest,
    ) -> impl Future<Output = Result<(), MachineContainerRuntimeError>> + Send;

    fn run_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunRpcRequest,
    ) -> impl Future<Output = Result<MachineRunContainerOutcome, MachineContainerRuntimeError>> + Send;

    fn remove_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRemoveRpcRequest,
    ) -> impl Future<Output = Result<(), MachineContainerRuntimeError>> + Send;

    fn stop_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerStopRpcRequest,
    ) -> impl Future<Output = Result<(), MachineContainerRuntimeError>> + Send;
}

pub trait DataplanePreparer {
    fn prepare_dataplane(
        &mut self,
        request: DataplanePrepareRequest,
    ) -> impl Future<Output = Result<DataplanePrepareProviderReport, DataplanePrepareError>> + Send;
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

pub trait ActiveRouteCommitter {
    fn commit_active_route(
        &mut self,
        request: ActiveRouteCommitRequest,
    ) -> impl Future<Output = Result<ActiveRouteCommit, ActiveRouteCommitError>> + Send;
}

impl DeployOperationRecorder for crate::controllers::OperationControllers {
    async fn record_deploy_transition(
        &mut self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<(), DeployOperationRecordError> {
        self.repository()
            .record_deploy_transition(operation_id, transition)
            .await
            .map(|_| ())
            .map_err(DeployOperationRecordError::RecordTransition)
    }

    async fn record_deploy_evidence(
        &mut self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<(), DeployOperationRecordError> {
        self.repository()
            .record_deploy_evidence(operation_id, evidence)
            .await
            .map(|_| ())
            .map_err(DeployOperationRecordError::RecordEvidence)
    }
}

impl ActiveRouteCommitter for AsyncNatsCoreStateStore {
    async fn commit_active_route(
        &mut self,
        request: ActiveRouteCommitRequest,
    ) -> Result<ActiveRouteCommit, ActiveRouteCommitError> {
        AsyncNatsCoreStateStore::commit_active_route(self, &request)
            .await
            .map_err(ActiveRouteCommitError::Store)
    }
}

#[derive(Debug)]
pub enum ActiveRouteCommitError {
    Store(ActiveRouteStoreError),
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
