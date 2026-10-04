//! Global catch-up: place observed eligible Globals onto this Machine only. Drain's
//! retirement of chosen Globals on one Machine shares its slot primitives.

use ployz_core::{
    BridgeEndpointCapacity, ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    CreateContainerRequest, EnvironmentValues, InspectRequest, ListContainersRequest, LiveServices,
    Machine, MachineId, MachineTarget, Namespace, ObservedGlobalSlotSpec, QualifiedService,
    ResolvedServiceSpec, RpcError, ServiceName, ServiceObservation, ServicePlacementEligibility,
    op, service_containers,
};

use tokio_util::sync::CancellationToken;

use crate::{connect::Client, deploy::endpoint_capacity_error, failure::Failure};

/// Catch-up failed after membership committed.
#[derive(Debug)]
pub(crate) struct CatchUpError {
    cause: Failure,
    unresolved: Vec<QualifiedService>,
}

impl CatchUpError {
    /// The `--json` code of the underlying failure.
    pub(crate) fn code(&self) -> ployz_core::RpcErrorCode {
        self.cause.report().code
    }

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
    async fn live_services(
        &mut self,
        environment: EnvironmentValues,
    ) -> Result<LiveServices<RpcError>, Failure>;
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
    /// Stop and remove `slot`'s Containers on `machine_id` when fresh evidence says it is
    /// definitely ineligible there; refuse when it is eligible or its eligibility unknown.
    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
    ) -> Result<(), RpcError>;
    /// List Containers directly from the joined Machine for final verification.
    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure>;
}

impl CatchUpClient for Client {
    async fn live_services(
        &mut self,
        environment: EnvironmentValues,
    ) -> Result<LiveServices<RpcError>, Failure> {
        Client::live_services(self, environment)
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
        let eligibility =
            slot_eligibility(self, machine_id, &request.namespace, &request.resolved_spec).await?;
        if eligibility != ServicePlacementEligibility::Eligible {
            if matches!(eligibility, ServicePlacementEligibility::Ineligible(_)) {
                remove_slot(
                    self,
                    machine_id,
                    &request.namespace,
                    &request.resolved_spec.name,
                )
                .await?;
                return Ok(None);
            }
            return Err(unresolved_eligibility(&eligibility));
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
            .map_err(|error| RpcError {
                code: ployz_core::RpcErrorCode::Unavailable,
                message: error.to_string(),
                details: serde_json::Value::Null,
            })?;
        if let Some(error) = endpoint_capacity_error(1, capacity.as_ref()) {
            return Err(RpcError {
                code: ployz_core::RpcErrorCode::Conflict,
                message: error.to_string(),
                details: serde_json::Value::Null,
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

    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
    ) -> Result<(), RpcError> {
        let identity = slot.identity();
        match slot_eligibility(self, machine_id, &identity.namespace, slot.resolved_spec()).await? {
            ServicePlacementEligibility::Ineligible(_) => {
                remove_slot(self, machine_id, &identity.namespace, &identity.name).await
            }
            ServicePlacementEligibility::Eligible => Err(RpcError {
                code: ployz_core::RpcErrorCode::Conflict,
                message: "the Server accepts it again".into(),
                details: serde_json::Value::Null,
            }),
            eligibility @ ServicePlacementEligibility::Unknown(_) => {
                Err(unresolved_eligibility(&eligibility))
            }
        }
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

/// Fresh eligibility of `spec` on `machine_id`, from that Machine's own observation.
async fn slot_eligibility(
    client: &mut Client,
    machine_id: &MachineId,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<ServicePlacementEligibility, RpcError> {
    let details = client
        .read::<op::Inspect>(
            InspectRequest {
                include_storage: true,
                ..Default::default()
            },
            &MachineTarget::from(machine_id),
        )
        .await?;
    let machine = details
        .machine
        .filter(|machine| {
            machine.id == *machine_id
                && details.phase == ployz_core::LocalMachinePhase::Participating
        })
        .ok_or_else(|| RpcError {
            code: ployz_core::RpcErrorCode::Conflict,
            message: "Global catch-up target has no participating Machine observation".into(),
            details: serde_json::Value::Null,
        })?;
    Ok(spec.placement_eligibility_in_namespace(namespace, &machine, details.storage.as_ref()))
}

/// Stop and remove every Service Container of `namespace`/`name` on `machine_id`.
async fn remove_slot(
    client: &mut Client,
    machine_id: &MachineId,
    namespace: &Namespace,
    name: &ServiceName,
) -> Result<(), RpcError> {
    let target = MachineTarget::from(machine_id);
    let containers = client
        .read::<op::ListContainers>(
            ListContainersRequest {
                environment: EnvironmentValues::Redacted,
            },
            &target,
        )
        .await?;
    for container in containers.containers.into_iter().filter(|container| {
        container.machine_id == *machine_id
            && container.kind == ContainerKind::ServiceContainer
            && container.namespace == *namespace
            && container.resolved_spec.name == *name
    }) {
        crate::ingress::stop_container(
            client,
            machine_id,
            ployz_core::StopContainerRequest {
                container_id: container.container_id,
                signal: None,
                grace_period_seconds: None,
            },
            None,
        )
        .await?;
        client
            .call::<op::RemoveContainer>(
                ployz_core::RemoveContainerRequest {
                    container_id: container.container_id,
                    remove_volumes: false,
                    force: false,
                },
                Some(&target),
            )
            .await
            .map_err(RpcError::from)?;
    }
    Ok(())
}

fn unresolved_eligibility(eligibility: &ServicePlacementEligibility) -> RpcError {
    RpcError {
        code: ployz_core::RpcErrorCode::Conflict,
        message: format!("Global catch-up target eligibility is {eligibility:?}"),
        details: serde_json::Value::Null,
    }
}

pub(crate) fn joined_catch_up_error(error: CatchUpError, server: &Machine) -> String {
    let mut message = format!(
        "Server joined, but Global catch-up is incomplete; it remains a Cluster member. {}",
        error.cause
    );
    if !error.unresolved.is_empty() {
        message.push_str("\nGlobals requiring attention:");
        for identity in error.unresolved {
            if identity == QualifiedService::system_ingress() {
                message.push_str(&format!(
                    "\n- ployz-system/ingress: run `ployz server set {} --accepts-ingress=true`.",
                    server.id
                ));
            } else {
                message.push_str(&format!(
                    "\n- {identity}: redeploy Namespace Service `{identity}`."
                ));
            }
        }
    }
    message
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
) -> Result<(), CatchUpError> {
    let live = client
        .live_services(EnvironmentValues::Included)
        .await
        .map_err(|error| CatchUpError::new(error, Vec::new()))?;
    if !live.containers.all_targets_succeeded() {
        return Err(CatchUpError::new(
            Failure::unavailable(format!(
                "Global catch-up cannot plan from partial Service observations: {}; restore peer connectivity and redeploy",
                crate::failure::partial_failure_details(&live.containers)
            )),
            Vec::new(),
        ));
    }
    let slots = live
        .services()
        .iter()
        .filter_map(ServiceObservation::observed_global_slot)
        .collect::<Vec<_>>();
    let identities = slots
        .iter()
        .map(|slot| slot.identity().clone())
        .collect::<Vec<_>>();
    let mut expected = Vec::new();
    let mut failures = Vec::new();
    for slot in slots {
        let identity = slot.identity().clone();
        let request = CreateContainerRequest {
            deployment_id: None,
            creation_key: Some(crate::cluster::global_creation_key(slot.resolved_spec())),
            kind: ContainerKind::ServiceContainer,
            namespace: identity.namespace.clone(),
            registry_auth: None,
            resolved_spec: slot.resolved_spec().clone(),
        };
        match client.create_slot(&this_machine.id, request).await {
            Ok(Some(created)) => {
                expected.push(slot);
                if let Err(error) = client
                    .start_slot(&this_machine.id, created.container_id)
                    .await
                {
                    failures.push((identity, error.to_string()));
                }
            }
            Ok(None) => {}
            Err(error) => failures.push((identity, error.to_string())),
        }
    }
    let target_containers = client
        .target_containers(&this_machine.id)
        .await
        .map_err(|error| CatchUpError::new(error, identities))?;
    let target_services = service_containers(target_containers);
    let mut missing = expected
        .into_iter()
        .filter_map(|slot| {
            (!slot.is_running_on(&target_services, this_machine)).then(|| slot.identity().clone())
        })
        .collect::<Vec<_>>();
    for (identity, _) in &failures {
        if !missing.contains(identity) {
            missing.push(identity.clone());
        }
    }
    if !missing.is_empty() {
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
        return Err(CatchUpError::new(cause, missing));
    }
    Ok(())
}

/// What retiring one Global on the drained Server came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Retirement {
    /// Its removal there was acknowledged, or a full observation found none of it left.
    Retired,
    /// It may still run there, and why.
    NotRetired(String),
    /// Cancellation came first.
    NotAttempted,
}

/// Retire `globals` on `server` only, through Global slot convergence: each is stopped
/// and removed there when fresh evidence says the Server rules it out, and held when its
/// eligibility is unknown. It starts nothing and touches no other Global or Server, and
/// stops at the next Global once `cancellation` fires.
///
/// Returns each of `globals`, in order, with its retirement. The caller observes what
/// still runs.
pub(crate) async fn retire_globals<C: CatchUpClient>(
    client: &mut C,
    server: &Machine,
    globals: &[QualifiedService],
    cancellation: &CancellationToken,
) -> Vec<(QualifiedService, Retirement)> {
    let everyone = |error: String| {
        globals
            .iter()
            .map(|identity| (identity.clone(), Retirement::NotRetired(error.clone())))
            .collect()
    };
    let live = match client.live_services(EnvironmentValues::Redacted).await {
        Ok(live) => live,
        Err(error) => return everyone(error.to_string()),
    };
    if !live.containers.all_targets_succeeded() {
        return everyone(format!(
            "Global retirement cannot plan from partial Service observations: {}",
            crate::failure::partial_failure_details(&live.containers)
        ));
    }
    let services = live.services();
    let mut retirements = Vec::with_capacity(globals.len());
    for global in globals {
        let retirement = if cancellation.is_cancelled() {
            Retirement::NotAttempted
        } else {
            match services.iter().find(|service| service.identity == *global) {
                None => Retirement::Retired,
                Some(service) => match service.observed_global_slot() {
                    None => Retirement::NotRetired(
                        "its newest Container is not Global; deploy it first".into(),
                    ),
                    Some(slot) => match client.retire_slot(&server.id, &slot).await {
                        Ok(()) => Retirement::Retired,
                        Err(error) => Retirement::NotRetired(error.to_string()),
                    },
                },
            }
        };
        retirements.push((global.clone(), retirement));
    }
    retirements
}

#[cfg(test)]
#[path = "global_catch_up_tests.rs"]
mod tests;
