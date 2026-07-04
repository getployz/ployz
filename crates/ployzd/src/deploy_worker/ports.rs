use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareRequest, PloyzNativeMeshPrepareReport,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::ControlPlaneCommitScope;
use ployz_core::ops::{DeployEvidence, DeployTransition, RouteTarget};
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, RouteBindingStoreError};
use std::future::Future;

use crate::machine_runtime::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerStopRpcRequest, MachineEnsureEndpointNetworkRpcRequest,
    MachineRunContainerOutcome,
};

use super::{
    DeployContainer, DeployHealthCheckError, DeployOperationRecordError,
    MachineContainerRuntimeError, ServingTargetCommitError,
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
    ) -> impl Future<Output = Result<(), RouteBindingCommitError>> + Send;

    fn remove_route_binding(
        &mut self,
        target: RouteTarget,
    ) -> impl Future<Output = Result<(), RouteBindingCommitError>> + Send;

    fn replace_serving_target_entry(
        &mut self,
        state: ServingTargetEntry,
    ) -> impl Future<Output = Result<(), ServingTargetCommitError>> + Send;

    fn remove_serving_target_entry(
        &mut self,
        entry: ServingTargetEntry,
    ) -> impl Future<Output = Result<(), ServingTargetCommitError>> + Send;
}

fn commit_scope(entry: &ServingTargetEntry) -> ControlPlaneCommitScope {
    ControlPlaneCommitScope::ServiceEntry {
        service_id: entry.service_id.clone(),
        namespace_revision_entry_id: entry.namespace_revision_entry_id.clone(),
    }
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

impl NamespaceStateCommitter for AsyncNatsCoreStateStore {
    async fn replace_route_binding(
        &mut self,
        state: RouteBindingState,
    ) -> Result<(), RouteBindingCommitError> {
        let target = state.target.clone();
        AsyncNatsCoreStateStore::replace_route_binding(self, &state)
            .await
            .map_err(|error| RouteBindingCommitError::Store { target, error })
    }

    async fn remove_route_binding(
        &mut self,
        target: RouteTarget,
    ) -> Result<(), RouteBindingCommitError> {
        AsyncNatsCoreStateStore::remove_route_binding(self, &target)
            .await
            .map_err(|error| RouteBindingCommitError::Store { target, error })
    }

    async fn replace_serving_target_entry(
        &mut self,
        state: ServingTargetEntry,
    ) -> Result<(), ServingTargetCommitError> {
        let scope = commit_scope(&state);
        AsyncNatsCoreStateStore::replace_serving_target_entry(self, &state)
            .await
            .map_err(|error| ServingTargetCommitError::Store { scope, error })
    }

    async fn remove_serving_target_entry(
        &mut self,
        entry: ServingTargetEntry,
    ) -> Result<(), ServingTargetCommitError> {
        let scope = commit_scope(&entry);
        AsyncNatsCoreStateStore::remove_serving_target_entry(
            self,
            &entry.namespace_id,
            &entry.service_id,
        )
        .await
        .map_err(|error| ServingTargetCommitError::Store { scope, error })
    }
}

#[derive(Debug)]
pub enum RouteBindingCommitError {
    Store {
        target: RouteTarget,
        error: RouteBindingStoreError,
    },
    NamespaceLockLost {
        target: RouteTarget,
    },
}
