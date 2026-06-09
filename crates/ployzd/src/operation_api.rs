//! User-facing operation service handlers.

use crate::backup_runtime::BackupOperationRuntime;
use crate::controllers::{
    BackupCreateCommand, DeploySubmitCommand, MachineAddBootstrapMaterial,
    MachineAddBootstrapMaterialError, MachineAddSubmitCommand, OperationControllers,
};
use crate::deploy_runtime::DeployOperationRuntime;
use crate::node_rpc::{NatsNodeLogsTailer, NodeLogsTailRuntimeError};
use crate::node_runtime_types::NodeLogsTailRequest as NodeRuntimeLogsTailRequest;
use ployz_core::ids::{ContainerId, NodeId, OperationId};
use ployz_core::machine::{
    MachineName, RawJoinToken, active_machine_from_completed_add, plan_first_node_activation,
};
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationOwnerLease, OperationStatus,
    OperationStatusSnapshot, ProjectionOperationState, StatusProjectionError,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_core::state::ActiveMachineState;
use ployz_core::state::ActiveServiceState;
use ployz_core::subjects::op_watch;
use ployz_nats::core_state::{
    ActiveMachineReadError, ActiveMachineWriteError, AsyncNatsCoreStateStore,
};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operations::{
    MachineJoinRedemption, OperationEventLogError, OperationEventReplayReadError,
    OperationStatusReadError, OperationStatusStoreError, RecordMachineAddEventError,
    RecordMachineJoinReportError,
    RedeemMachineJoinTokenError as RedeemMachineJoinTokenRepositoryError,
    ReplayOperationEventsError, SubmitBackupError as SubmitBackupRepositoryError,
    SubmitDeployError as SubmitDeployRepositoryError,
    SubmitMachineAddError as SubmitMachineAddRepositoryError,
};
use ployz_sdk_types::{
    AcceptedOperation, BackupCreateError, BackupCreateRequest, BackupCreateUnavailableSource,
    BootstrapMaterialFailure, DeploySubmitError, DeploySubmitRequest,
    DeploySubmitUnavailableSource, EventReplayFailure, InitFirstNodeActivateError,
    InitFirstNodeActivateRequest, InitFirstNodeActivated, LogsTailError, LogsTailRequest,
    LogsTailResult, LogsTailUnavailableSource, MachineAddAccepted, MachineAddError,
    MachineAddRequest, MachineAddUnavailableSource, MachineInspectError, MachineInspectRequest,
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemResult,
    MachineJoinRedeemUnavailableSource, MachineJoinRedeemed, MachineJoinReportError,
    MachineJoinReportFailure, MachineJoinReportOutcome, MachineJoinReportRequest,
    MachineJoinReportUnavailableSource, MachineJoinReported, MachineJoinToken, MachineListError,
    MachineListRequest, MachineListResult, MachineQueryUnavailableSource, MachineSnapshot,
    OperationSubmitClockFailure, OperationSubmitEventFailure, OperationSubmitStatusFailure,
    OpsStatusError, OpsStatusUnavailableSource, OpsWatchError, OpsWatchUnavailableSource,
    ServiceInspectError, ServiceInspectRequest, ServiceListError, ServiceListRequest,
    ServiceListResult, ServiceQueryUnavailableSource, ServiceSnapshot, StatusReadFailure,
};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct OperationApiHandlers {
    controllers: OperationControllers,
    deploy_execution: DeploySubmitExecution,
    backup_execution: BackupCreateExecution,
    machine_query: MachineQueryExecution,
    service_query: ServiceQueryExecution,
    logs_query: LogsQueryExecution,
}

impl OperationApiHandlers {
    #[must_use]
    pub fn accept_only(controllers: OperationControllers) -> Self {
        Self {
            controllers,
            deploy_execution: DeploySubmitExecution::AcceptOnly,
            backup_execution: BackupCreateExecution::AcceptOnly,
            machine_query: MachineQueryExecution::Unavailable,
            service_query: ServiceQueryExecution::Unavailable,
            logs_query: LogsQueryExecution::Unavailable,
        }
    }

    #[must_use]
    pub fn execute_operations(
        controllers: OperationControllers,
        deploy_runtime: DeployOperationRuntime,
        backup_runtime: BackupOperationRuntime,
        machine_query: MachineQueryRuntime,
        logs_tailer: NatsNodeLogsTailer,
    ) -> Self {
        let service_query = ServiceQueryRuntime::new(machine_query.core_state.clone());
        let logs_query = LogsQueryRuntime::new(machine_query.observations.clone(), logs_tailer);
        Self {
            controllers,
            deploy_execution: DeploySubmitExecution::Execute(Arc::new(deploy_runtime)),
            backup_execution: BackupCreateExecution::Execute(Arc::new(backup_runtime)),
            machine_query: MachineQueryExecution::Execute(Arc::new(machine_query)),
            service_query: ServiceQueryExecution::Execute(Arc::new(service_query)),
            logs_query: LogsQueryExecution::Execute(Arc::new(logs_query)),
        }
    }

    #[must_use]
    pub const fn controllers(&self) -> &OperationControllers {
        &self.controllers
    }
}

#[derive(Clone)]
pub enum DeploySubmitExecution {
    AcceptOnly,
    Execute(Arc<DeployOperationRuntime>),
}

impl DeploySubmitExecution {
    fn start_owned(&self, accepted: crate::controllers::AcceptedDeployOperation) {
        match (self, accepted.should_start_execution) {
            (Self::Execute(runtime), true) => runtime.start(accepted),
            (Self::AcceptOnly, true | false) | (Self::Execute(_), false) => {}
        }
    }
}

#[derive(Clone)]
pub enum BackupCreateExecution {
    AcceptOnly,
    Execute(Arc<BackupOperationRuntime>),
}

impl BackupCreateExecution {
    fn start_owned(&self, accepted: crate::controllers::AcceptedBackupOperation) {
        match (self, accepted.should_start_execution) {
            (Self::Execute(runtime), true) => runtime.start(accepted),
            (Self::AcceptOnly, true | false) | (Self::Execute(_), false) => {}
        }
    }
}

#[derive(Clone)]
pub enum MachineQueryExecution {
    Unavailable,
    Execute(Arc<MachineQueryRuntime>),
}

#[derive(Clone)]
pub enum ServiceQueryExecution {
    Unavailable,
    Execute(Arc<ServiceQueryRuntime>),
}

#[derive(Clone)]
pub enum LogsQueryExecution {
    Unavailable,
    Execute(Arc<LogsQueryRuntime>),
}

#[derive(Clone)]
pub struct MachineQueryRuntime {
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
}

#[derive(Clone)]
pub struct ServiceQueryRuntime {
    core_state: AsyncNatsCoreStateStore,
}

#[derive(Clone)]
pub struct LogsQueryRuntime {
    observations: AsyncNatsObservationStore,
    tailer: NatsNodeLogsTailer,
}

impl LogsQueryRuntime {
    #[must_use]
    pub const fn new(observations: AsyncNatsObservationStore, tailer: NatsNodeLogsTailer) -> Self {
        Self {
            observations,
            tailer,
        }
    }

    async fn tail(&self, request: LogsTailRequest) -> Result<LogsTailResult, LogsTailError> {
        let node_id = match request.node_id.clone() {
            Some(node_id) => {
                self.verify_observed_container_on_node(&node_id, &request.container_id)
                    .await?;
                node_id
            }
            None => self
                .find_container_node(&request.container_id)
                .await?
                .ok_or_else(|| LogsTailError::NoSuchContainer {
                    container_id: request.container_id.clone(),
                })?,
        };

        self.tailer
            .tail_logs(NodeRuntimeLogsTailRequest {
                node_id,
                container_id: request.container_id,
                tail_lines: request.tail_lines.map(|lines| lines.get()),
            })
            .await
            .map(|value| LogsTailResult {
                node_id: value.node_id,
                container_id: value.container_id,
                text: value.text,
                truncated: value.truncated,
            })
            .map_err(logs_tail_node_error)
    }

    async fn find_container_node(
        &self,
        container_id: &ContainerId,
    ) -> Result<Option<NodeId>, LogsTailError> {
        let mut matches = self
            .observations
            .node_snapshot_records()
            .await
            .map_err(|_| LogsTailError::Unavailable {
                source: LogsTailUnavailableSource::Observations,
                node_id: None,
            })?
            .into_iter()
            .filter_map(|record| {
                record
                    .snapshot
                    .container(container_id)
                    .map(|_| record.snapshot.node_id().clone())
            })
            .collect::<Vec<_>>();
        matches.sort();
        matches.dedup();

        match matches.as_slice() {
            [] => Ok(None),
            [node_id] => Ok(Some(node_id.clone())),
            node_ids => Err(LogsTailError::AmbiguousContainer {
                container_id: container_id.clone(),
                node_ids: node_ids.to_vec(),
            }),
        }
    }

    async fn verify_observed_container_on_node(
        &self,
        node_id: &NodeId,
        container_id: &ContainerId,
    ) -> Result<(), LogsTailError> {
        let Some(snapshot) = self
            .observations
            .node_snapshot(node_id)
            .await
            .map_err(|_| LogsTailError::Unavailable {
                source: LogsTailUnavailableSource::Observations,
                node_id: Some(node_id.clone()),
            })?
        else {
            return Err(LogsTailError::NoSuchContainer {
                container_id: container_id.clone(),
            });
        };
        if snapshot.container(container_id).is_none() {
            return Err(LogsTailError::NoSuchContainer {
                container_id: container_id.clone(),
            });
        }
        Ok(())
    }
}

impl ServiceQueryRuntime {
    #[must_use]
    pub const fn new(core_state: AsyncNatsCoreStateStore) -> Self {
        Self { core_state }
    }

    async fn list(&self) -> Result<ServiceListResult, ServiceListError> {
        let services = self
            .core_state
            .active_services()
            .await
            .map_err(service_list_core_error)?
            .into_iter()
            .map(service_snapshot)
            .collect();
        Ok(ServiceListResult { services })
    }

    async fn inspect(
        &self,
        service_id: &ployz_core::ids::ServiceId,
    ) -> Result<ServiceSnapshot, ServiceInspectError> {
        let Some(active) = self
            .core_state
            .active_service(service_id)
            .await
            .map_err(service_inspect_core_error)?
        else {
            return Err(ServiceInspectError::NoSuchService {
                service_id: service_id.clone(),
            });
        };

        Ok(service_snapshot(active))
    }
}

fn service_snapshot(active: ActiveServiceState) -> ServiceSnapshot {
    ServiceSnapshot { active }
}

impl MachineQueryRuntime {
    #[must_use]
    pub fn new(
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
    ) -> Self {
        Self {
            core_state,
            observations,
        }
    }

    async fn list(&self) -> Result<MachineListResult, MachineListError> {
        let machines = self
            .core_state
            .active_machines()
            .await
            .map_err(machine_list_core_error)?;
        let public_ips = self
            .observations
            .node_public_ips()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|observation| (observation.node_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let gateway_statuses = self
            .observations
            .gateway_statuses()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|observation| (observation.node_id.clone(), observation))
            .collect::<BTreeMap<_, _>>();
        let container_counts = self
            .observations
            .node_snapshot_records()
            .await
            .map_err(|_| MachineListError::Unavailable {
                source: MachineQueryUnavailableSource::Observations,
            })?
            .into_iter()
            .map(|record| {
                (
                    record.snapshot.node_id().clone(),
                    record.snapshot.containers().len(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut snapshots = Vec::with_capacity(machines.len());
        for machine in machines {
            snapshots.push(MachineSnapshot {
                public_ip: public_ips.get(&machine.node_id).cloned(),
                gateway: gateway_statuses.get(&machine.node_id).cloned(),
                observed_container_count: container_counts
                    .get(&machine.node_id)
                    .copied()
                    .unwrap_or_default(),
                active: machine,
            });
        }
        Ok(MachineListResult {
            machines: snapshots,
        })
    }

    async fn inspect(&self, node_id: &NodeId) -> Result<MachineSnapshot, MachineInspectError> {
        let Some(machine) = self
            .core_state
            .active_machine(node_id)
            .await
            .map_err(machine_inspect_core_error)?
        else {
            return Err(MachineInspectError::NoSuchMachine {
                node_id: node_id.clone(),
            });
        };

        self.snapshot(machine).await.map_err(machine_inspect_error)
    }

    async fn activate_machine(
        &self,
        machine: &ActiveMachineState,
    ) -> Result<(), ActiveMachineWriteError> {
        self.core_state.replace_active_machine(machine).await
    }

    async fn snapshot(
        &self,
        active: ActiveMachineState,
    ) -> Result<MachineSnapshot, MachineSnapshotError> {
        let public_ip = self
            .observations
            .node_public_ip(&active.node_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?;
        let gateway = self
            .observations
            .gateway_status(&active.node_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?;
        let observed_container_count = self
            .observations
            .node_snapshot(&active.node_id)
            .await
            .map_err(|_| MachineSnapshotError::Observations)?
            .map(|snapshot| snapshot.containers().len())
            .unwrap_or_default();

        Ok(MachineSnapshot {
            active,
            public_ip,
            gateway,
            observed_container_count,
        })
    }
}

enum MachineSnapshotError {
    Observations,
}

pub async fn machine_list(
    handlers: &OperationApiHandlers,
    _request: MachineListRequest,
) -> Result<MachineListResult, MachineListError> {
    let MachineQueryExecution::Execute(runtime) = &handlers.machine_query else {
        return Err(MachineListError::Unavailable {
            source: MachineQueryUnavailableSource::CoreState,
        });
    };
    runtime.list().await
}

pub async fn machine_inspect(
    handlers: &OperationApiHandlers,
    request: MachineInspectRequest,
) -> Result<MachineSnapshot, MachineInspectError> {
    let MachineQueryExecution::Execute(runtime) = &handlers.machine_query else {
        return Err(MachineInspectError::Unavailable {
            source: MachineQueryUnavailableSource::CoreState,
        });
    };
    runtime.inspect(&request.node_id).await
}

pub async fn service_list(
    handlers: &OperationApiHandlers,
    _request: ServiceListRequest,
) -> Result<ServiceListResult, ServiceListError> {
    let ServiceQueryExecution::Execute(runtime) = &handlers.service_query else {
        return Err(ServiceListError::Unavailable {
            source: ServiceQueryUnavailableSource::CoreState,
        });
    };
    runtime.list().await
}

pub async fn service_inspect(
    handlers: &OperationApiHandlers,
    request: ServiceInspectRequest,
) -> Result<ServiceSnapshot, ServiceInspectError> {
    let ServiceQueryExecution::Execute(runtime) = &handlers.service_query else {
        return Err(ServiceInspectError::Unavailable {
            source: ServiceQueryUnavailableSource::CoreState,
        });
    };
    runtime.inspect(&request.service_id).await
}

pub async fn logs_tail(
    handlers: &OperationApiHandlers,
    request: LogsTailRequest,
) -> Result<LogsTailResult, LogsTailError> {
    let LogsQueryExecution::Execute(runtime) = &handlers.logs_query else {
        return Err(LogsTailError::Unavailable {
            source: LogsTailUnavailableSource::NodeRpc,
            node_id: None,
        });
    };
    runtime.tail(request).await
}

fn logs_tail_node_error(error: NodeLogsTailRuntimeError) -> LogsTailError {
    match error {
        NodeLogsTailRuntimeError::NotFound { container_id, .. } => {
            LogsTailError::NoSuchContainer { container_id }
        }
        NodeLogsTailRuntimeError::ReadFailed {
            node_id,
            container_id,
            message,
        } => LogsTailError::ReadFailed {
            node_id,
            container_id,
            message,
        },
        NodeLogsTailRuntimeError::Unavailable { node_id, .. } => LogsTailError::Unavailable {
            source: LogsTailUnavailableSource::NodeRpc,
            node_id: Some(node_id),
        },
    }
}

fn machine_list_core_error(_error: ActiveMachineReadError) -> MachineListError {
    MachineListError::Unavailable {
        source: MachineQueryUnavailableSource::CoreState,
    }
}

fn machine_inspect_core_error(_error: ActiveMachineReadError) -> MachineInspectError {
    MachineInspectError::Unavailable {
        source: MachineQueryUnavailableSource::CoreState,
    }
}

fn machine_inspect_error(error: MachineSnapshotError) -> MachineInspectError {
    match error {
        MachineSnapshotError::Observations => MachineInspectError::Unavailable {
            source: MachineQueryUnavailableSource::Observations,
        },
    }
}

fn service_list_core_error(
    _error: ployz_nats::core_state::CoreStateStoreError,
) -> ServiceListError {
    ServiceListError::Unavailable {
        source: ServiceQueryUnavailableSource::CoreState,
    }
}

fn service_inspect_core_error(
    _error: ployz_nats::core_state::CoreStateStoreError,
) -> ServiceInspectError {
    ServiceInspectError::Unavailable {
        source: ServiceQueryUnavailableSource::CoreState,
    }
}

#[must_use]
pub fn owned_operation(
    operation_id: OperationId,
    start_sequence: ployz_core::ops::EventSequence,
    lease: OperationOwnerLease,
) -> AcceptedOperation {
    let watch_subject = op_watch(&operation_id);
    AcceptedOperation {
        operation_id,
        watch_subject,
        start_sequence,
        owner_lease: lease,
    }
}

impl From<DeploySubmitRequest> for DeploySubmitCommand {
    fn from(value: DeploySubmitRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            idempotency_key: value.idempotency_key,
            target: value.target,
        }
    }
}

impl From<BackupCreateRequest> for BackupCreateCommand {
    fn from(value: BackupCreateRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            idempotency_key: value.idempotency_key,
        }
    }
}

pub async fn deploy_submit(
    handlers: &OperationApiHandlers,
    command: DeploySubmitCommand,
) -> Result<AcceptedOperation, DeploySubmitError> {
    let operation_id = command.operation_id.clone();
    let accepted = handlers
        .controllers
        .submit_deploy(command)
        .await
        .map_err(|error| deploy_submit_error_from_submit_error(operation_id, error))?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        accepted.start_sequence,
        accepted.lease.clone(),
    );
    handlers.deploy_execution.start_owned(accepted);

    Ok(operation)
}

pub async fn machine_add(
    controllers: &OperationControllers,
    request: MachineAddRequest,
) -> Result<MachineAddAccepted, MachineAddError> {
    let operation_id = request.operation_id.clone();
    let material = machine_add_bootstrap_material(controllers, &request)
        .await
        .map_err(|error| MachineAddError::Unavailable {
            operation_id: operation_id.clone(),
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: bootstrap_material_failure(error),
            },
        })?;
    let bootstrap_nats_url = material.join_bundle.material.runtime_nats_url.clone();
    let command = MachineAddSubmitCommand {
        operation_id: request.operation_id,
        idempotency_key: request.idempotency_key,
        node_id: request.node_id,
        name: request.name,
        gateway: first_node_gateway(request.gateway),
        join_bundle: material.join_bundle,
        secret_delivery: material.secret_delivery,
        join_token: material.join_token,
        raw_join_token: material.raw_join_token,
    };

    let accepted = controllers
        .submit_machine_add(command)
        .await
        .map_err(|error| machine_add_error_from_submit_error(operation_id.clone(), error))?;
    let raw_token = MachineJoinToken::try_new(accepted.raw_join_token.as_str()).map_err(|_| {
        MachineAddError::Unavailable {
            operation_id: operation_id.clone(),
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: BootstrapMaterialFailure::IssueJoinToken,
            },
        }
    })?;

    Ok(MachineAddAccepted {
        accepted: owned_operation(
            accepted.operation_id,
            accepted.start_sequence,
            accepted.lease,
        ),
        node_id: accepted.node_id,
        bootstrap_url: material.bootstrap_url,
        bootstrap_nats_url,
        join_token: raw_token,
    })
}

pub async fn init_first_node_activate(
    handlers: &OperationApiHandlers,
    request: InitFirstNodeActivateRequest,
) -> Result<InitFirstNodeActivated, InitFirstNodeActivateError> {
    let plan = plan_first_node_activation(&request.node_id)
        .map_err(|_| InitFirstNodeActivateError::InvalidPlan)?;
    if let Some(active) = first_node_active_machine(handlers, &request.node_id).await? {
        return Ok(InitFirstNodeActivated {
            operation_id: active.activated_by,
            node_id: active.node_id,
        });
    }
    let accepted = machine_add(
        handlers.controllers(),
        MachineAddRequest {
            operation_id: plan.operation_id,
            idempotency_key: plan.idempotency_key,
            node_id: request.node_id,
            name: plan.name,
            gateway: request.gateway,
        },
    )
    .await
    .map_err(|failure| InitFirstNodeActivateError::MachineAdd { failure })?;
    machine_join_redeem(
        handlers.controllers(),
        MachineJoinRedeemRequest {
            join_token: accepted.join_token.clone(),
        },
    )
    .await
    .map_err(|failure| InitFirstNodeActivateError::JoinRedeem { failure })?;
    let reported = machine_join_report(
        handlers,
        MachineJoinReportRequest {
            join_token: accepted.join_token,
            outcome: MachineJoinReportOutcome::Completed,
        },
    )
    .await
    .map_err(|failure| InitFirstNodeActivateError::JoinReport { failure })?;

    Ok(InitFirstNodeActivated {
        operation_id: reported.operation_id,
        node_id: reported.node_id,
    })
}

async fn first_node_active_machine(
    handlers: &OperationApiHandlers,
    node_id: &NodeId,
) -> Result<Option<ActiveMachineState>, InitFirstNodeActivateError> {
    let MachineQueryExecution::Execute(runtime) = &handlers.machine_query else {
        return Err(InitFirstNodeActivateError::Unavailable {
            source: MachineQueryUnavailableSource::CoreState,
        });
    };
    runtime
        .core_state
        .active_machine(node_id)
        .await
        .map_err(|_| InitFirstNodeActivateError::Unavailable {
            source: MachineQueryUnavailableSource::CoreState,
        })
}

async fn machine_add_bootstrap_material(
    controllers: &OperationControllers,
    request: &MachineAddRequest,
) -> Result<MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError> {
    let Some(existing) = controllers
        .submitted_machine_add_bootstrap_material(&request.idempotency_key)
        .await?
    else {
        return controllers.issue_machine_add_bootstrap_material(&request.operation_id);
    };
    Ok(MachineAddBootstrapMaterial {
        raw_join_token: existing.raw_join_token,
        join_token: existing.join_token,
        bootstrap_url: controllers.machine_bootstrap_url().clone(),
        join_bundle: existing.join_bundle,
        secret_delivery: existing.secret_delivery,
    })
}

pub async fn backup_create(
    handlers: &OperationApiHandlers,
    request: BackupCreateRequest,
) -> Result<AcceptedOperation, BackupCreateError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers
        .submit_backup(request.into())
        .await
        .map_err(|error| backup_create_error_from_submit_error(operation_id, error))?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        accepted.start_sequence,
        accepted.lease.clone(),
    );
    handlers.backup_execution.start_owned(accepted);

    Ok(operation)
}

pub async fn machine_join_redeem(
    controllers: &OperationControllers,
    request: MachineJoinRedeemRequest,
) -> Result<MachineJoinRedeemed, MachineJoinRedeemError> {
    let raw_token = RawJoinToken::try_new(request.join_token.as_str())
        .map_err(|_| MachineJoinRedeemError::InvalidJoinToken)?;
    let redeemed = controllers
        .redeem_machine_join_token(&raw_token)
        .await
        .map_err(machine_join_redeem_error_from_repository_error)?;
    Ok(machine_join_redeemed(redeemed))
}

pub async fn machine_join_report(
    handlers: &OperationApiHandlers,
    request: MachineJoinReportRequest,
) -> Result<MachineJoinReported, MachineJoinReportError> {
    let raw_token = RawJoinToken::try_new(request.join_token.as_str())
        .map_err(|_| MachineJoinReportError::InvalidJoinToken)?;
    let outcome = request.outcome;
    let result = match outcome.clone() {
        MachineJoinReportOutcome::Completed => {
            handlers
                .controllers
                .record_machine_join_completed(&raw_token)
                .await
        }
        MachineJoinReportOutcome::Failed { failure } => {
            handlers
                .controllers
                .record_machine_join_failed(
                    &raw_token,
                    machine_add_failure_from_join_report_failure(failure),
                )
                .await
        }
    };
    let reported = match result {
        Ok(reported) => reported,
        Err(error) if matches!(outcome, MachineJoinReportOutcome::Completed) => {
            if let Some(reported) = repair_completed_machine_join_report(handlers, &error).await? {
                return Ok(reported);
            }
            return Err(machine_join_report_error(error));
        }
        Err(error) => return Err(machine_join_report_error(error)),
    };

    let status = handlers
        .controllers
        .operation_status(&reported.operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::StatusRead {
                failure: status_read_failure(&error),
            },
        })?
        .ok_or(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        })?;

    let OperationStatus::MachineAdd {
        last_event_sequence,
        name,
        node_id,
        ..
    } = status
    else {
        return Err(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        });
    };
    if let MachineJoinReportOutcome::Completed = outcome {
        activate_reported_machine(handlers, &reported.operation_id, &node_id, &name).await?;
    }

    Ok(MachineJoinReported {
        operation_id: reported.operation_id,
        node_id: reported.node_id,
        last_event_sequence,
        outcome,
    })
}

async fn repair_completed_machine_join_report(
    handlers: &OperationApiHandlers,
    error: &RecordMachineJoinReportError,
) -> Result<Option<MachineJoinReported>, MachineJoinReportError> {
    let Some(operation_id) = completed_machine_add_operation_id(error) else {
        return Ok(None);
    };
    let Some(status) = handlers
        .controllers
        .operation_status(&operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::StatusRead {
                failure: status_read_failure(&error),
            },
        })?
    else {
        return Err(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        });
    };
    let OperationStatus::MachineAdd {
        id,
        node_id,
        name,
        state: ployz_core::machine::MachineAddOperationState::Completed,
        last_event_sequence,
        ..
    } = status
    else {
        return Ok(None);
    };

    activate_reported_machine(handlers, &id, &node_id, &name).await?;
    Ok(Some(MachineJoinReported {
        operation_id: id,
        node_id,
        last_event_sequence,
        outcome: MachineJoinReportOutcome::Completed,
    }))
}

fn completed_machine_add_operation_id(error: &RecordMachineJoinReportError) -> Option<OperationId> {
    let RecordMachineJoinReportError::RecordMachineAddEvent(
        RecordMachineAddEventError::ProjectStatus(
            StatusProjectionError::InvalidTransition {
                operation_id,
                current,
                ..
            }
            | StatusProjectionError::TerminalState {
                operation_id,
                current,
                ..
            },
        ),
    ) = error
    else {
        return None;
    };
    let ProjectionOperationState::MachineAdd(state) = current.as_ref() else {
        return None;
    };
    if state.name() == ployz_core::machine::MachineAddOperationStateName::Completed {
        Some(operation_id.clone())
    } else {
        None
    }
}

async fn activate_reported_machine(
    handlers: &OperationApiHandlers,
    operation_id: &OperationId,
    node_id: &NodeId,
    name: &MachineName,
) -> Result<(), MachineJoinReportError> {
    let MachineQueryExecution::Execute(runtime) = &handlers.machine_query else {
        return Err(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        });
    };
    let active_machine = active_machine_from_completed_add(
        operation_id.clone(),
        node_id.clone(),
        name.clone(),
        ployz_core::machine::MachineAddOperationState::Completed,
    )
    .map_err(|_| MachineJoinReportError::Unavailable {
        source: MachineJoinReportUnavailableSource::OperationCorrupt,
    })?;
    runtime
        .activate_machine(&active_machine)
        .await
        .map_err(|_| MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::CoreState,
        })
}

fn machine_add_failure_from_join_report_failure(
    failure: MachineJoinReportFailure,
) -> ployz_core::machine::MachineAddFailure {
    match failure {
        MachineJoinReportFailure::BootstrapFailed { message } => {
            ployz_core::machine::MachineAddFailure::BootstrapFailed { message }
        }
    }
}

fn machine_join_redeemed(redemption: MachineJoinRedemption) -> MachineJoinRedeemed {
    let (joined, result) = match redemption {
        MachineJoinRedemption::Joined(joined) => (joined, MachineJoinRedeemResult::Joined),
        MachineJoinRedemption::AlreadyJoined(joined) => {
            (joined, MachineJoinRedeemResult::AlreadyJoined)
        }
    };

    MachineJoinRedeemed {
        operation_id: joined.operation_id,
        node_id: joined.node_id,
        name: joined.name,
        gateway: joined.gateway,
        join_bundle: joined.join_bundle,
        secret_delivery: joined.secret_delivery,
        joined_at: joined.joined_at,
        last_event_sequence: joined.last_event_sequence,
        result,
    }
}

fn machine_join_report_error(error: RecordMachineJoinReportError) -> MachineJoinReportError {
    match &error {
        RecordMachineJoinReportError::InvalidJoinToken => {
            return MachineJoinReportError::InvalidJoinToken;
        }
        RecordMachineJoinReportError::UnknownJoinToken => {
            return MachineJoinReportError::UnknownJoinToken;
        }
        RecordMachineJoinReportError::RecordMachineAddEvent(error) => match error {
            RecordMachineAddEventError::ProjectStatus(
                StatusProjectionError::InvalidTransition {
                    operation_id,
                    current,
                    ..
                }
                | StatusProjectionError::TerminalState {
                    operation_id,
                    current,
                    ..
                },
            ) => {
                if let ProjectionOperationState::MachineAdd(state) = current.as_ref() {
                    return MachineJoinReportError::OperationNotJoining {
                        operation_id: operation_id.clone(),
                        current: state.name(),
                    };
                }
            }
            RecordMachineAddEventError::LoadStatus(_)
            | RecordMachineAddEventError::StoreStatus(_)
            | RecordMachineAddEventError::MissingOperation { .. }
            | RecordMachineAddEventError::ProjectStatus(_)
            | RecordMachineAddEventError::AppendEvent(_)
            | RecordMachineAddEventError::StoredEventMismatch { .. }
            | RecordMachineAddEventError::StatusCursorContended => {}
        },
        RecordMachineJoinReportError::StoreStatus(_)
        | RecordMachineJoinReportError::JoinTokenMismatch { .. } => {}
    }

    MachineJoinReportError::Unavailable {
        source: record_machine_join_report_unavailable_source(&error),
    }
}

fn machine_join_redeem_error_from_repository_error(
    error: RedeemMachineJoinTokenRepositoryError,
) -> MachineJoinRedeemError {
    match error {
        RedeemMachineJoinTokenRepositoryError::Clock { .. } => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::Clock {
                    failure: OperationSubmitClockFailure::BeforeUnixEpoch,
                },
            }
        }
        RedeemMachineJoinTokenRepositoryError::InvalidJoinToken => {
            MachineJoinRedeemError::InvalidJoinToken
        }
        RedeemMachineJoinTokenRepositoryError::UnknownJoinToken => {
            MachineJoinRedeemError::UnknownJoinToken
        }
        RedeemMachineJoinTokenRepositoryError::LoadStatus(source) => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::StatusRead {
                    failure: status_read_failure(&source),
                },
            }
        }
        RedeemMachineJoinTokenRepositoryError::StoreStatus(source) => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::StatusWrite {
                    failure: operation_submit_status_failure(&source),
                },
            }
        }
        RedeemMachineJoinTokenRepositoryError::RecordMachineAddEvent(source) => {
            MachineJoinRedeemError::Unavailable {
                source: record_machine_add_event_unavailable_source(&source),
            }
        }
        RedeemMachineJoinTokenRepositoryError::MissingOperation { .. }
        | RedeemMachineJoinTokenRepositoryError::MissingSecretDelivery { .. }
        | RedeemMachineJoinTokenRepositoryError::WrongOperationKind { .. }
        | RedeemMachineJoinTokenRepositoryError::JoinTokenMismatch { .. } => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::OperationCorrupt,
            }
        }
        RedeemMachineJoinTokenRepositoryError::OperationNotPending {
            operation_id,
            current,
        } => MachineJoinRedeemError::OperationNotPending {
            operation_id,
            current,
        },
        RedeemMachineJoinTokenRepositoryError::JoinRejected {
            operation_id,
            failure,
        } => MachineJoinRedeemError::Rejected {
            operation_id,
            failure,
        },
    }
}

fn deploy_submit_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitDeployRepositoryError,
) -> DeploySubmitError {
    match error {
        SubmitDeployRepositoryError::AppendEvent(source) => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::EventLog {
                failure: operation_submit_event_failure(&source),
            },
        },
        SubmitDeployRepositoryError::StoreStatus(source) => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::StatusStore {
                failure: operation_submit_status_failure(&source),
            },
        },
        SubmitDeployRepositoryError::Clock { .. } => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::Clock {
                failure: OperationSubmitClockFailure::BeforeUnixEpoch,
            },
        },
        SubmitDeployRepositoryError::DuplicateSequenceMismatch { sequence } => {
            DeploySubmitError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
    }
}

fn backup_create_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitBackupRepositoryError,
) -> BackupCreateError {
    match error {
        SubmitBackupRepositoryError::AppendEvent(source) => BackupCreateError::Unavailable {
            operation_id,
            source: BackupCreateUnavailableSource::EventLog {
                failure: operation_submit_event_failure(&source),
            },
        },
        SubmitBackupRepositoryError::StoreStatus(source) => BackupCreateError::Unavailable {
            operation_id,
            source: BackupCreateUnavailableSource::StatusStore {
                failure: operation_submit_status_failure(&source),
            },
        },
        SubmitBackupRepositoryError::Clock { .. } => BackupCreateError::Unavailable {
            operation_id,
            source: BackupCreateUnavailableSource::Clock {
                failure: OperationSubmitClockFailure::BeforeUnixEpoch,
            },
        },
        SubmitBackupRepositoryError::DuplicateSequenceMismatch { sequence } => {
            BackupCreateError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
    }
}

fn machine_add_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitMachineAddRepositoryError,
) -> MachineAddError {
    match error {
        SubmitMachineAddRepositoryError::AppendEvent(source) => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::EventLog {
                failure: operation_submit_event_failure(&source),
            },
        },
        SubmitMachineAddRepositoryError::StoreStatus(source) => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::StatusStore {
                failure: operation_submit_status_failure(&source),
            },
        },
        SubmitMachineAddRepositoryError::Clock { .. } => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::Clock {
                failure: OperationSubmitClockFailure::BeforeUnixEpoch,
            },
        },
        SubmitMachineAddRepositoryError::JoinTokenMismatch => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: BootstrapMaterialFailure::IssueJoinToken,
            },
        },
        SubmitMachineAddRepositoryError::DuplicateSequenceMismatch { sequence } => {
            MachineAddError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
    }
}

#[must_use]
pub fn ops_status_missing(operation_id: &OperationId) -> OpsStatusError {
    OpsStatusError::NoSuchOperation {
        operation_id: operation_id.clone(),
    }
}

pub async fn ops_status(
    controllers: &OperationControllers,
    operation_id: OperationId,
) -> Result<OperationStatusSnapshot, OpsStatusError> {
    match controllers.operation_status_snapshot(&operation_id).await {
        Ok(Some(snapshot)) => Ok(snapshot),
        Ok(None) => Err(ops_status_missing(&operation_id)),
        Err(error) => Err(OpsStatusError::Unavailable {
            operation_id,
            source: OpsStatusUnavailableSource::StatusStore {
                failure: status_store_read_failure(&error),
            },
        }),
    }
}

fn ops_watch_error_from_replay_error(
    operation_id: OperationId,
    error: ReplayOperationEventsError,
) -> OpsWatchError {
    match error {
        ReplayOperationEventsError::MissingOperation { operation_id } => {
            OpsWatchError::NoSuchOperation { operation_id }
        }
        ReplayOperationEventsError::LoadStatus(source) => OpsWatchError::Unavailable {
            operation_id,
            source: OpsWatchUnavailableSource::StatusStore {
                failure: status_read_failure(&source),
            },
        },
        ReplayOperationEventsError::ReadEvents(source) => OpsWatchError::Unavailable {
            operation_id,
            source: OpsWatchUnavailableSource::EventLog {
                failure: event_replay_failure(&source),
            },
        },
    }
}

pub async fn ops_watch(
    controllers: &OperationControllers,
    request: OperationEventReplayRequest,
) -> Result<OperationEventReplayPage, OpsWatchError> {
    let operation_id = request.operation_id.clone();
    controllers
        .replay_operation_events(request)
        .await
        .map_err(|error| ops_watch_error_from_replay_error(operation_id, error))
}

fn operation_submit_status_failure(
    error: &OperationStatusStoreError,
) -> OperationSubmitStatusFailure {
    match error {
        OperationStatusStoreError::OpenBucket { .. } => OperationSubmitStatusFailure::OpenBucket,
        OperationStatusStoreError::EncodeStatus(_) => OperationSubmitStatusFailure::EncodeStatus,
        OperationStatusStoreError::DecodeStatus(_) => OperationSubmitStatusFailure::DecodeStatus,
        OperationStatusStoreError::EncodeSubmission(_) => {
            OperationSubmitStatusFailure::EncodeSubmission
        }
        OperationStatusStoreError::DecodeSubmission(_) => {
            OperationSubmitStatusFailure::DecodeSubmission
        }
        OperationStatusStoreError::EncodeLease(_) => OperationSubmitStatusFailure::EncodeLease,
        OperationStatusStoreError::DecodeLease(_) => OperationSubmitStatusFailure::DecodeLease,
        OperationStatusStoreError::CasConflict { .. } => OperationSubmitStatusFailure::CasConflict,
        OperationStatusStoreError::GetStatus { .. } => OperationSubmitStatusFailure::GetStatus,
        OperationStatusStoreError::Clock { .. } => OperationSubmitStatusFailure::Clock,
        OperationStatusStoreError::Timeout { .. } => OperationSubmitStatusFailure::Timeout,
    }
}

fn operation_submit_event_failure(error: &OperationEventLogError) -> OperationSubmitEventFailure {
    match error {
        OperationEventLogError::EncodeEvent(_) => OperationSubmitEventFailure::EncodeEvent,
        OperationEventLogError::DecodeEvent(_) => OperationSubmitEventFailure::DecodeEvent,
        OperationEventLogError::PublishRequest { .. } => {
            OperationSubmitEventFailure::PublishRequest
        }
        OperationEventLogError::PublishAck { .. } => OperationSubmitEventFailure::PublishAck,
        OperationEventLogError::ReadEvent { .. } => OperationSubmitEventFailure::ReadEvent,
        OperationEventLogError::Timeout { .. } => OperationSubmitEventFailure::Timeout,
        OperationEventLogError::InvalidAckSequence { .. } => {
            OperationSubmitEventFailure::InvalidAckSequence
        }
    }
}

fn record_machine_add_event_unavailable_source(
    error: &RecordMachineAddEventError,
) -> MachineJoinRedeemUnavailableSource {
    match error {
        RecordMachineAddEventError::LoadStatus(error) => {
            MachineJoinRedeemUnavailableSource::StatusRead {
                failure: status_read_failure(error),
            }
        }
        RecordMachineAddEventError::StoreStatus(error) => {
            MachineJoinRedeemUnavailableSource::StatusWrite {
                failure: operation_submit_status_failure(error),
            }
        }
        RecordMachineAddEventError::AppendEvent(error) => {
            MachineJoinRedeemUnavailableSource::EventLog {
                failure: operation_submit_event_failure(error),
            }
        }
        RecordMachineAddEventError::MissingOperation { .. }
        | RecordMachineAddEventError::ProjectStatus(_)
        | RecordMachineAddEventError::StoredEventMismatch { .. }
        | RecordMachineAddEventError::StatusCursorContended => {
            MachineJoinRedeemUnavailableSource::OperationCorrupt
        }
    }
}

fn record_machine_join_report_unavailable_source(
    error: &RecordMachineJoinReportError,
) -> MachineJoinReportUnavailableSource {
    match error {
        RecordMachineJoinReportError::StoreStatus(error) => {
            MachineJoinReportUnavailableSource::StatusWrite {
                failure: operation_submit_status_failure(error),
            }
        }
        RecordMachineJoinReportError::RecordMachineAddEvent(error) => {
            record_machine_add_report_unavailable_source(error)
        }
        RecordMachineJoinReportError::InvalidJoinToken
        | RecordMachineJoinReportError::UnknownJoinToken
        | RecordMachineJoinReportError::JoinTokenMismatch { .. } => {
            MachineJoinReportUnavailableSource::OperationCorrupt
        }
    }
}

fn record_machine_add_report_unavailable_source(
    error: &RecordMachineAddEventError,
) -> MachineJoinReportUnavailableSource {
    match error {
        RecordMachineAddEventError::LoadStatus(error) => {
            MachineJoinReportUnavailableSource::StatusRead {
                failure: status_read_failure(error),
            }
        }
        RecordMachineAddEventError::StoreStatus(error) => {
            MachineJoinReportUnavailableSource::StatusWrite {
                failure: operation_submit_status_failure(error),
            }
        }
        RecordMachineAddEventError::AppendEvent(error) => {
            MachineJoinReportUnavailableSource::EventLog {
                failure: operation_submit_event_failure(error),
            }
        }
        RecordMachineAddEventError::MissingOperation { .. }
        | RecordMachineAddEventError::ProjectStatus(_)
        | RecordMachineAddEventError::StoredEventMismatch { .. }
        | RecordMachineAddEventError::StatusCursorContended => {
            MachineJoinReportUnavailableSource::OperationCorrupt
        }
    }
}

fn status_read_failure(error: &OperationStatusReadError) -> StatusReadFailure {
    match error {
        OperationStatusReadError::DecodeStatus(_) => StatusReadFailure::DecodeStatus,
        OperationStatusReadError::GetStatus { .. } => StatusReadFailure::GetStatus,
        OperationStatusReadError::Timeout { .. } => StatusReadFailure::Timeout,
    }
}

fn status_store_read_failure(error: &OperationStatusStoreError) -> StatusReadFailure {
    match error {
        OperationStatusStoreError::DecodeStatus(_) => StatusReadFailure::DecodeStatus,
        OperationStatusStoreError::DecodeLease(_) => StatusReadFailure::DecodeLease,
        OperationStatusStoreError::GetStatus { .. } => StatusReadFailure::GetStatus,
        OperationStatusStoreError::Clock { .. } => StatusReadFailure::Clock,
        OperationStatusStoreError::Timeout { .. } => StatusReadFailure::Timeout,
        OperationStatusStoreError::OpenBucket { .. }
        | OperationStatusStoreError::EncodeStatus(_)
        | OperationStatusStoreError::EncodeSubmission(_)
        | OperationStatusStoreError::DecodeSubmission(_)
        | OperationStatusStoreError::EncodeLease(_)
        | OperationStatusStoreError::CasConflict { .. } => StatusReadFailure::GetStatus,
    }
}

fn first_node_gateway(gateway: ployz_sdk_types::MachineAddGateway) -> FirstNodeGateway {
    match gateway {
        ployz_sdk_types::MachineAddGateway::Install => FirstNodeGateway::Install,
        ployz_sdk_types::MachineAddGateway::Skip => FirstNodeGateway::Skip,
    }
}

fn bootstrap_material_failure(error: MachineAddBootstrapMaterialError) -> BootstrapMaterialFailure {
    match error {
        MachineAddBootstrapMaterialError::Clock { .. }
        | MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial
        | MachineAddBootstrapMaterialError::StatusRead { .. } => {
            BootstrapMaterialFailure::IssueJoinToken
        }
        MachineAddBootstrapMaterialError::MissingJoinTemplate => {
            BootstrapMaterialFailure::MissingJoinTemplate
        }
    }
}

fn event_replay_failure(error: &OperationEventReplayReadError) -> EventReplayFailure {
    match error {
        OperationEventReplayReadError::DecodeEvent(_) => EventReplayFailure::DecodeEvent,
        OperationEventReplayReadError::ReadEvent { .. } => EventReplayFailure::ReadEvent,
        OperationEventReplayReadError::Timeout { .. } => EventReplayFailure::Timeout,
        OperationEventReplayReadError::InvalidEventSequence { .. } => {
            EventReplayFailure::InvalidEventSequence
        }
        OperationEventReplayReadError::InvalidNextReplaySequence { .. } => {
            EventReplayFailure::InvalidNextReplaySequence
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        deploy_submit_error_from_submit_error, ops_watch_error_from_replay_error,
        status_read_failure,
    };
    use ployz_core::ids::OperationId;
    use ployz_core::ops::EventSequence;
    use ployz_nats::operations::{
        OperationEventLogError, OperationEventReplayReadError, OperationStatusReadError,
        OperationStatusStoreError, ReplayOperationEventsError,
        SubmitDeployError as SubmitDeployRepositoryError,
    };
    use ployz_sdk_types::{
        DeploySubmitError, DeploySubmitUnavailableSource, EventReplayFailure,
        OperationSubmitEventFailure, OperationSubmitStatusFailure, OpsWatchError,
        OpsWatchUnavailableSource, StatusReadFailure,
    };

    #[test]
    fn deploy_submit_maps_status_store_failure_to_api_error() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                operation_id.clone(),
                SubmitDeployRepositoryError::StoreStatus(OperationStatusStoreError::CasConflict {
                    message: "contended".to_owned(),
                }),
            ),
            DeploySubmitError::Unavailable {
                operation_id,
                source: DeploySubmitUnavailableSource::StatusStore {
                    failure: OperationSubmitStatusFailure::CasConflict,
                },
            }
        );
    }

    #[test]
    fn deploy_submit_maps_event_log_failure_to_api_error() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                operation_id.clone(),
                SubmitDeployRepositoryError::AppendEvent(OperationEventLogError::PublishRequest {
                    message: "publish unavailable".to_owned(),
                }),
            ),
            DeploySubmitError::Unavailable {
                operation_id,
                source: DeploySubmitUnavailableSource::EventLog {
                    failure: OperationSubmitEventFailure::PublishRequest,
                },
            }
        );
    }

    #[test]
    fn deploy_submit_preserves_duplicate_sequence_mismatch() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                operation_id.clone(),
                SubmitDeployRepositoryError::DuplicateSequenceMismatch {
                    sequence: event_sequence(9),
                },
            ),
            DeploySubmitError::DuplicateSequenceMismatch {
                operation_id,
                sequence: event_sequence(9),
            }
        );
    }

    #[test]
    fn ops_watch_maps_missing_operation_to_api_error() {
        let operation_id = operation_id("op_missing");

        assert_eq!(
            ops_watch_error_from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::MissingOperation {
                    operation_id: operation_id.clone(),
                },
            ),
            OpsWatchError::NoSuchOperation { operation_id }
        );
    }

    #[test]
    fn ops_watch_preserves_status_store_failure_context() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            ops_watch_error_from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::LoadStatus(OperationStatusReadError::GetStatus {
                    message: "kv unavailable".to_owned(),
                }),
            ),
            OpsWatchError::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::StatusStore {
                    failure: StatusReadFailure::GetStatus,
                },
            }
        );
    }

    #[test]
    fn ops_watch_preserves_event_log_failure_context() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            ops_watch_error_from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::ReadEvents(OperationEventReplayReadError::ReadEvent {
                    message: "stream unavailable".to_owned(),
                }),
            ),
            OpsWatchError::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::EventLog {
                    failure: EventReplayFailure::ReadEvent,
                },
            }
        );
    }

    #[test]
    fn ops_status_preserves_status_store_failure_context() {
        assert_eq!(
            status_read_failure(&OperationStatusReadError::Timeout { operation: "test" }),
            StatusReadFailure::Timeout
        );
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn event_sequence(value: u64) -> EventSequence {
        EventSequence::try_new(value).expect("valid event sequence")
    }
}
