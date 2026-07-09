use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareRequest, PloyzNativeMeshPrepareReport,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::ControlPlaneCommitScope;
use ployz_core::ops::{DeployEvidence, DeployTransition, RouteTarget};
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use std::future::Future;

use crate::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRestartRpcRequest,
    MachineContainerRunRpcRequest, MachineContainerStopRpcRequest,
    MachineEnsureEndpointNetworkRpcRequest, MachineRunContainerOutcome,
};

use super::{
    DeployContainer, DeployHealthCheckError, DeployOperationRecordError,
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

    fn restart_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRestartRpcRequest,
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
    ) -> impl Future<Output = Result<PloyzNativeMeshPrepareReport, DataplanePrepareError>> + Send;
}

pub trait DeployHealthChecker {
    fn wait_healthy(
        &mut self,
        containers: &[DeployContainer],
    ) -> impl Future<Output = Result<(), DeployHealthCheckError>> + Send;
}

/// The one deploy commit port: every namespace-state write (route bindings
/// and serving-target entries) goes through this seam, fenced by the
/// Namespace Lock in the production adapter.
pub trait NamespaceStateCommitter {
    fn replace_route_binding(
        &mut self,
        state: RouteBindingState,
    ) -> impl Future<Output = Result<(), NamespaceCommitError>> + Send;

    fn remove_route_binding(
        &mut self,
        target: RouteTarget,
    ) -> impl Future<Output = Result<(), NamespaceCommitError>> + Send;

    fn replace_serving_target_entry(
        &mut self,
        state: ServingTargetEntry,
    ) -> impl Future<Output = Result<(), NamespaceCommitError>> + Send;

    fn remove_serving_target_entry(
        &mut self,
        entry: ServingTargetEntry,
    ) -> impl Future<Output = Result<(), NamespaceCommitError>> + Send;
}

/// One error for every namespace-state commit; variants carry the subject
/// each concern is keyed by (route targets vs commit scopes).
#[derive(Debug)]
pub enum NamespaceCommitError {
    RouteStore {
        target: RouteTarget,
        message: String,
    },
    RouteLockLost {
        target: RouteTarget,
    },
    ServingTargetStore {
        scope: ControlPlaneCommitScope,
        message: String,
    },
    ServingTargetLockLost {
        scope: ControlPlaneCommitScope,
    },
}

impl DeployOperationRecorder for crate::operation_api::admission::OperationControllers {
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
