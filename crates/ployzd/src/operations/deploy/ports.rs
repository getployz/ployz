use ployz_core::cert::ActiveCertState;
use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareRequest, PloyzNativeMeshPrepareReport,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::{ImageEnsureOk, ImageEnsureRequest};
use ployz_core::ops::ControlPlaneCommitScope;
use ployz_core::ops::{
    CertificateProvisionFailure, DeployEvidence, DeployTransition, RouteHostname, RouteTarget,
};
use ployz_core::state::{RouteBindingState, ServingTargetEntry, VolumePinState};
use std::future::Future;

use crate::certificate::GatewayCertificateTarget;

use crate::roles::machine::client::{MachineImageEnsureError, MachineImageResolveError};
use crate::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerResolveImageRpcRequest,
    MachineContainerRestartRpcRequest, MachineContainerRunHookRpcOk,
    MachineContainerRunHookRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerStopRpcRequest, MachineEnsureEndpointNetworkRpcRequest,
    MachineRunContainerOutcome,
};

use super::{
    DeployContainer, DeployHealthCheckError, DeployOperationRecordError,
    MachineContainerRuntimeError, PreStartHookRuntimeError,
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
    fn resolve_image(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerResolveImageRpcRequest,
    ) -> impl Future<Output = Result<ployz_core::image::OciDigest, MachineImageResolveError>> + Send;

    fn ensure_image(
        &mut self,
        machine_id: &MachineId,
        request: ImageEnsureRequest,
    ) -> impl Future<Output = Result<ImageEnsureOk, MachineImageEnsureError>> + Send;

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

    fn run_pre_start_hook(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunHookRpcRequest,
    ) -> impl Future<Output = Result<MachineContainerRunHookRpcOk, PreStartHookRuntimeError>> + Send;

    fn remove_pre_start_hook(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRemoveRpcRequest,
    ) -> impl Future<Output = Result<(), PreStartHookRuntimeError>> + Send;

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

pub trait CertificateProvisioner {
    fn ensure(
        &mut self,
        owner_operation_id: &OperationId,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
    ) -> impl Future<Output = Result<ActiveCertState, CertificateProvisionFailure>> + Send;
}

/// The one deploy commit port: every namespace-state write (route bindings
/// and serving-target entries) goes through this seam, fenced by the
/// Namespace Lock in the production adapter.
pub trait NamespaceStateCommitter {
    fn commit_deploy_phase(
        &mut self,
        route_bindings: Vec<RouteBindingState>,
        serving_target_entries: Vec<ServingTargetEntry>,
    ) -> impl Future<Output = Result<(), NamespaceCommitError>> + Send;

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

    fn replace_volume_pin(
        &mut self,
        state: VolumePinState,
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
    VolumePinStore {
        state: VolumePinState,
        message: String,
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
