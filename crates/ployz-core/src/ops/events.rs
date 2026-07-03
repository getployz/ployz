//! Durable operation events and their subjects.

use serde::{Deserialize, Serialize};

use crate::cert::{AcmeHttp01Challenge, ActiveCertState};
use crate::dataplane::PloyzNativeMeshPrepareReport;
use crate::deploy::{DeployCleanupContainer, DeployPlan, DeployRequest};
use crate::ids::{CertId, ContainerId, MachineId, OperationId, ServiceId};
use crate::install::InstallArtifactVersion;
use crate::machine::{
    IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineCredentialProvisioningStep,
    MachineName,
};
use crate::roles::InstallRolePolicy;

use super::text::CancellationReason;
use super::{
    CertOperationFailure, DeployCleanupFailure, DeployCompletionOutcome, DeployOperationFailure,
    DeployRunningStage, MachineSubstrateVersions, MachineUpdateFailure,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSubject {
    Deploy { service_id: ServiceId },
    Cert { cert_id: CertId },
    MachineAdd { machine_id: MachineId },
    MachineUpdate { machine_id: MachineId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationEvent {
    DeploySubmitted {
        operation_id: OperationId,
        target: DeployRequest,
    },
    DeployPlanningStarted {
        operation_id: OperationId,
    },
    DeployPlanCreated {
        operation_id: OperationId,
        plan: DeployPlan,
    },
    DeployRunning {
        operation_id: OperationId,
        stage: DeployRunningStage,
    },
    DeployDataplanePrepared {
        operation_id: OperationId,
        report: PloyzNativeMeshPrepareReport,
    },
    DeployContainerStarted {
        operation_id: OperationId,
        machine_id: MachineId,
        container_id: ContainerId,
    },
    DeployHealthCheckStarted {
        operation_id: OperationId,
    },
    DeployCleanupFinished {
        operation_id: OperationId,
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
    },
    DeployCompleted {
        operation_id: OperationId,
        outcome: DeployCompletionOutcome,
    },
    DeployFailed {
        operation_id: OperationId,
        failure: DeployOperationFailure,
    },
    CertRenewalSubmitted {
        operation_id: OperationId,
        cert_id: CertId,
    },
    CertChallengePublished {
        operation_id: OperationId,
        cert_id: CertId,
        challenge: AcmeHttp01Challenge,
    },
    CertValidationStarted {
        operation_id: OperationId,
        cert_id: CertId,
    },
    CertCompleted {
        operation_id: OperationId,
        active_cert: ActiveCertState,
    },
    CertFailed {
        operation_id: OperationId,
        failure: CertOperationFailure,
    },
    MachineAddSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        join_token: IssuedJoinToken,
    },
    MachineAddJoined {
        operation_id: OperationId,
        machine_id: MachineId,
        joined_at: JoinTokenRedeemedAt,
    },
    MachineAddCredentialProvisioned {
        operation_id: OperationId,
        machine_id: MachineId,
        step: MachineCredentialProvisioningStep,
    },
    MachineAddCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineAddFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineAddFailure,
    },
    MachineUpdateSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
    },
    MachineUpdateRunning {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineUpdateCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
        reported: MachineSubstrateVersions,
    },
    MachineUpdateFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineUpdateFailure,
    },
    Cancelled {
        operation_id: OperationId,
        reason: CancellationReason,
    },
}
