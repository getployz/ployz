//! Global catch-up: place observed eligible Globals onto this Machine only.

use ployz_core::{
    BridgeEndpointCapacity, ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    CreateContainerRequest, EnvironmentValues, InspectRequest, ListContainersRequest, LiveServices,
    Machine, MachineId, MachineTarget, QualifiedService, RpcError, ServiceObservation,
    ServicePlacementEligibility, op, service_containers,
};

use crate::global_slot::{remove_slot, slot_eligibility, unknown_eligibility};
use crate::{connect::Client, deploy::endpoint_capacity_error, failure::Failure};

/// Catch-up failed after membership committed.
#[derive(Debug)]
pub(crate) struct CatchUpError {
    cause: Failure,
    unresolved: Vec<QualifiedService>,
}

impl CatchUpError {
    /// Record the failure and Globals whose eligibility or running slot is unresolved.
    pub(crate) fn new(cause: Failure, unresolved: Vec<QualifiedService>) -> Self {
        Self { cause, unresolved }
    }
}

impl std::fmt::Display for CatchUpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause.fmt(formatter)
    }
}

pub(crate) trait CatchUpClient {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure>;
    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure>;
    async fn create_slot(
        &mut self,
        machine_id: &MachineId,
        request: CreateContainerRequest,
    ) -> Result<Option<ContainerCreated>, RpcError>;
    async fn start_slot(
        &mut self,
        machine_id: &MachineId,
        container_id: ContainerId,
    ) -> Result<(), RpcError>;
    /// List Containers directly from the joined Machine for final verification.
    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure>;
}

impl CatchUpClient for Client {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure> {
        Client::live_services(self, EnvironmentValues::Included)
            .await
            .map_err(Into::into)
    }

    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        let details = self
            .read::<op::Inspect>(
                InspectRequest {
                    telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                    ..Default::default()
                },
                &MachineTarget::from(machine_id),
            )
            .await
            .map_err(Failure::from)?;
        Ok(details.telemetry.map(|telemetry| telemetry.into_bridge()))
    }

    async fn create_slot(
        &mut self,
        machine_id: &MachineId,
        request: CreateContainerRequest,
    ) -> Result<Option<ContainerCreated>, RpcError> {
        let target = MachineTarget::from(machine_id);
        match slot_eligibility(self, machine_id, &request.namespace, &request.resolved_spec).await?
        {
            ServicePlacementEligibility::Eligible => {}
            ServicePlacementEligibility::Ineligible(_) => {
                remove_slot(
                    self,
                    machine_id,
                    &request.namespace,
                    &request.resolved_spec.name,
                )
                .await?;
                return Ok(None);
            }
            ServicePlacementEligibility::Unknown(reason) => {
                return Err(unknown_eligibility(reason));
            }
        }
        // Explicit Deploy replacement keys also distinguish the previous Container.
        // Reuse its exact persisted creation when catch-up finds it before Start.
        let containers = self
            .read::<op::ListContainers>(
                ListContainersRequest {
                    environment: EnvironmentValues::Included,
                },
                &target,
            )
            .await?;
        if let Some(existing) = containers.containers.into_iter().find(|container| {
            container.machine_id == *machine_id
                && container.kind == ContainerKind::ServiceContainer
                && container.namespace == request.namespace
                && container.resolved_spec == request.resolved_spec
        }) {
            return Ok(Some(ContainerCreated {
                container_id: existing.container_id,
                display_name: existing.display_name.clone(),
            }));
        }
        let capacity = self
            .bridge_capacity(machine_id)
            .await
            .map_err(|error| crate::ui::rpc_error(ployz_core::RpcErrorCode::Unavailable, &error))?;
        if let Some(error) = endpoint_capacity_error(1, capacity.as_ref()) {
            return Err(RpcError {
                code: ployz_core::RpcErrorCode::Conflict,
                message: error.to_string(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            });
        }
        self.call::<op::CreateContainer>(request, Some(&target))
            .await
            .map(Some)
            .map_err(Into::into)
    }

    async fn start_slot(
        &mut self,
        machine_id: &MachineId,
        container_id: ContainerId,
    ) -> Result<(), RpcError> {
        self.call::<op::StartContainer>(
            ployz_core::StartContainerRequest { container_id },
            Some(&MachineTarget::from(machine_id)),
        )
        .await
        .map(|_| ())
        .map_err(Into::into)
    }

    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure> {
        self.read::<op::ListContainers>(
            ListContainersRequest {
                environment: EnvironmentValues::Included,
            },
            &MachineTarget::from(machine_id),
        )
        .await
        .map(|list| list.containers)
        .map_err(Failure::from)
    }
}

pub(crate) fn joined_catch_up_error(
    error: CatchUpError,
    server: &Machine,
    command: impl Fn(&[&str]) -> String,
) -> Failure {
    let mut message = if error.cause.is_interrupted() {
        "Server joined; Global catch-up was interrupted. It remains a Cluster member."
    } else {
        "Server joined, but Global catch-up is incomplete; it remains a Cluster member."
    }
    .to_owned();
    for identity in &error.unresolved {
        if *identity != QualifiedService::system_ingress() {
            message.push_str(&format!(
                " To finish, redeploy Namespace Service `{identity}`."
            ));
        }
    }
    let mut failure = error.cause.context(message);
    for identity in error.unresolved {
        let hint = if identity == QualifiedService::system_ingress() {
            crate::ui::Hint::Retry(command(&[
                "server",
                "set",
                &server.id.to_string(),
                "--accepts-ingress=true",
            ]))
        } else {
            crate::ui::Hint::Inspect(command(&[
                "logs",
                &identity.to_string(),
                "--machine",
                &server.id.to_string(),
            ]))
        };
        failure = failure.hint(hint);
    }
    failure
}

/// A factual stage of target-only catch-up, adapted by the owning command.
pub(crate) enum CatchUpFact {
    Identified(Vec<QualifiedService>),
    Creating(QualifiedService),
    Starting(QualifiedService),
    Excluded(QualifiedService),
    Running(QualifiedService),
    Failed(QualifiedService),
}

/// Follow target-only catch-up with the shared progress block.
pub(crate) async fn follow_globals(
    client: &mut Client,
    assigned: &Machine,
) -> Result<(), CatchUpError> {
    use crate::ui::progress::{Disposition, Frame, Progress, Row, Run, State, Subject, Timing};
    let signal = crate::cancellation::interrupted()
        .map_err(|error| CatchUpError::new(error.into(), Vec::new()))?;
    let mut frame = Frame {
        run: Run::CatchUp(assigned.id),
        title: format!("Starting Globals on {}", assigned.name),
        rows: Vec::new(),
        notices: Vec::new(),
    };
    let mut progress = Progress::start(frame.clone());
    let result = catch_up_globals(client, assigned, &signal, |fact| {
        let (identity, state) = match fact {
            CatchUpFact::Identified(services) => {
                frame.rows = services
                    .into_iter()
                    .map(|service| Row {
                        subject: Subject::ServiceOnServer {
                            service,
                            machine: assigned.id,
                            server: assigned.name.to_string(),
                        },
                        state: State::Pending,
                        detail: None,
                        timing: Timing::Unavailable,
                    })
                    .collect();
                progress.update(frame.clone());
                return;
            }
            CatchUpFact::Creating(service) => (
                service,
                State::Running(ployz_store::RowPhase::CreatingContainer),
            ),
            CatchUpFact::Starting(service) => (
                service,
                State::Running(ployz_store::RowPhase::StartingContainer),
            ),
            CatchUpFact::Excluded(service) => (service, State::Excluded),
            CatchUpFact::Running(service) => (service, State::ObservedRunning),
            CatchUpFact::Failed(service) => (service, State::Failed),
        };
        if let Some(row) = frame.rows.iter_mut().find(|row| {
            matches!(&row.subject, Subject::ServiceOnServer { service, .. } if service == &identity)
        }) {
            if matches!(row.timing, Timing::Unavailable) {
                row.timing = Timing::Started(std::time::SystemTime::now());
            }
            if !matches!(state, State::Running(_) | State::Pending)
                && let Timing::Started(at) = row.timing
            {
                row.timing = Timing::Finished(at.elapsed().unwrap_or_default());
            }
            row.state = state;
        }
        progress.update(frame.clone());
    })
    .await;
    if result.is_err() {
        for row in &mut frame.rows {
            if matches!(row.state, State::Pending | State::Running(_)) {
                row.state = State::NotAttempted;
            }
        }
    }
    progress.finish(
        frame,
        if signal.is_cancelled() {
            Disposition::LocalInterrupted
        } else {
            Disposition::Settled
        },
    );
    result
}

/// Copy every observed eligible Global onto `this_machine` only.
///
/// # Errors
///
/// Fails when listing Services fails, the target Machine does not answer, or
/// any eligible Global cannot be placed, or required storage evidence is unknown.
pub(crate) async fn catch_up_globals<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    cancel: &tokio_util::sync::CancellationToken,
    mut observe: impl FnMut(CatchUpFact),
) -> Result<(), CatchUpError> {
    let live = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(CatchUpError::new(Failure::cancelled(), Vec::new())),
        live = client.live_services() => live.map_err(|error| CatchUpError::new(error, Vec::new()))?,
    };
    if !live.containers.all_targets_succeeded() {
        return Err(CatchUpError::new(
            Failure::unavailable(format!(
                "Global catch-up cannot plan from partial Service observations: {}; restore peer connectivity and redeploy",
                crate::failure::partial_failure_details(&live.containers, MachineId::to_string)
            )),
            Vec::new(),
        ));
    }
    let slots = live
        .services()
        .iter()
        .filter_map(ServiceObservation::observed_global_slot)
        .collect::<Vec<_>>();
    let mut unresolved = slots
        .iter()
        .map(|slot| slot.identity().clone())
        .collect::<Vec<_>>();
    observe(CatchUpFact::Identified(unresolved.clone()));
    let mut expected = Vec::new();
    let mut failures = Vec::new();
    for slot in slots {
        if cancel.is_cancelled() {
            break;
        }
        let identity = slot.identity().clone();
        let request = CreateContainerRequest {
            deployment_id: None,
            creation_key: Some(crate::cluster::global_creation_key(slot.resolved_spec())),
            kind: ContainerKind::ServiceContainer,
            namespace: identity.namespace.clone(),
            registry_auth: None,
            resolved_spec: slot.resolved_spec().clone(),
        };
        observe(CatchUpFact::Creating(identity.clone()));
        match client.create_slot(&this_machine.id, request).await {
            Ok(Some(created)) => {
                expected.push(slot);
                observe(CatchUpFact::Starting(identity.clone()));
                if let Err(error) = client
                    .start_slot(&this_machine.id, created.container_id)
                    .await
                {
                    observe(CatchUpFact::Failed(identity.clone()));
                    failures.push((identity, error.to_string()));
                }
            }
            Ok(None) => {
                unresolved.retain(|service| service != &identity);
                observe(CatchUpFact::Excluded(identity));
            }
            Err(error) => {
                observe(CatchUpFact::Failed(identity.clone()));
                failures.push((identity, crate::ui::row(&error)));
            }
        }
    }
    let target_containers = client
        .target_containers(&this_machine.id)
        .await
        .map_err(|error| {
            CatchUpError::new(
                if cancel.is_cancelled() {
                    error.interrupted()
                } else {
                    error
                },
                unresolved.clone(),
            )
        })?;
    let target_services = service_containers(target_containers);
    for slot in &expected {
        if slot.is_running_on(&target_services, this_machine) {
            if !failures
                .iter()
                .any(|(identity, _)| identity == slot.identity())
            {
                unresolved.retain(|identity| identity != slot.identity());
            }
            observe(CatchUpFact::Running(slot.identity().clone()));
        } else {
            observe(CatchUpFact::Failed(slot.identity().clone()));
        }
    }
    if cancel.is_cancelled() {
        return Err(CatchUpError::new(Failure::cancelled(), unresolved));
    }
    if !unresolved.is_empty() {
        let details = failures
            .iter()
            .map(|(identity, error)| format!("{identity}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        let cause = if details.is_empty() {
            Failure::unavailable("eligible Globals are not running after catch-up")
        } else {
            Failure::unavailable(format!("Global catch-up incomplete: {details}"))
        };
        return Err(CatchUpError::new(cause, unresolved));
    }
    Ok(())
}

#[cfg(test)]
#[path = "global_catch_up_tests.rs"]
mod tests;
