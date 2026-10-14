//! Retire a Drain's chosen Globals on the drained Server, one Global slot at a time.

use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerLocator, ContainerWant, Drain,
    EnvironmentValues, ListContainersRequest, LiveServices, Machine, MachineId, MachineName,
    MachineTarget, ObservedGlobalSlotSpec, QualifiedService, RpcError, RpcErrorCode,
    ServicePlacementEligibility, op,
};
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError};
use crate::global_slot::{slot_eligibility, unknown_eligibility};
use crate::serving_publish::{PointerClient, Protection, Rotation, amend_pointer, protection};

pub(crate) const POINTED_RETIRE_WAIT: Duration = Duration::from_secs(330);
const POINTED_RETIRE_POLL: Duration = Duration::from_secs(1);

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

/// What retirement needs from a Cluster.
pub(crate) trait RetireClient {
    /// What every Server runs, with each Server's name.
    async fn live_services(&mut self) -> Result<(LiveServices<RpcError>, Names), ConnectError>;
    /// Retire `slot`'s Containers on `machine_id` when fresh evidence says it is definitely
    /// ineligible there; refuse when it is eligible or its eligibility unknown.
    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError>;
}

pub(crate) type Names = Vec<(MachineId, MachineName)>;

impl RetireClient for Client {
    async fn live_services(&mut self) -> Result<(LiveServices<RpcError>, Names), ConnectError> {
        let machines = self.machines().await?;
        let live = self
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await?;
        let names = machines
            .into_iter()
            .map(|machine| (machine.machine.id, machine.machine.name))
            .collect();
        Ok((live, names))
    }

    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        let identity = slot.identity();
        match slot_eligibility(self, machine_id, &identity.namespace, slot.resolved_spec()).await? {
            ServicePlacementEligibility::Ineligible(_) => {
                retire_copies(
                    self,
                    machine_id,
                    identity,
                    POINTED_RETIRE_WAIT,
                    cancellation,
                )
                .await
            }
            ServicePlacementEligibility::Eligible => Err(RpcError {
                code: RpcErrorCode::Conflict,
                message: "the Server accepts it again".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            }),
            ServicePlacementEligibility::Unknown(reason) => Err(unknown_eligibility(reason)),
        }
    }
}

pub(crate) trait CopyLister: PointerClient {
    async fn copies_on(
        &mut self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerId>, RpcError>;
    async fn remove_copy(
        &mut self,
        machine_id: &MachineId,
        copy: &ContainerId,
    ) -> Result<(), RpcError>;
}

impl CopyLister for Client {
    async fn copies_on(
        &mut self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerId>, RpcError> {
        let list = self
            .read::<op::ListContainers>(
                ListContainersRequest {
                    environment: EnvironmentValues::Redacted,
                },
                &MachineTarget::from(machine_id),
            )
            .await?;
        Ok(list
            .containers
            .into_iter()
            .filter(|container| {
                container.machine_id == *machine_id
                    && container.kind == ContainerKind::ServiceContainer
                    && container.namespace == service.namespace
                    && container.resolved_spec.name == service.name
            })
            .map(|container| container.container_id)
            .collect())
    }

    async fn remove_copy(
        &mut self,
        machine_id: &MachineId,
        copy: &ContainerId,
    ) -> Result<(), RpcError> {
        self.ensure_container(
            machine_id,
            ContainerWant::Gone {
                container: ContainerLocator::Id(*copy),
                drain: Drain::DEFAULT,
                anonymous_volumes: AnonymousVolumes::Remove,
            },
            None,
        )
        .await
        .map(|_| ())
    }
}

pub(crate) async fn retire_copies<C: CopyLister>(
    client: &mut C,
    machine_id: &MachineId,
    service: &QualifiedService,
    wait: Duration,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    let deadline = tokio::time::Instant::now() + wait;
    let still_retiring = || {
        unavailable(format!(
            "still retiring after {} s; a Machine has not stopped routing to it",
            wait.as_secs()
        ))
    };
    let within = |deadline| {
        if tokio::time::Instant::now() >= deadline {
            Err(still_retiring())
        } else {
            Ok(())
        }
    };
    let mut generation: Option<Generation> = None;
    loop {
        within(deadline)?;
        let copies = cancellable(cancellation, client.copies_on(machine_id, service)).await?;
        let generation = generation.get_or_insert_with(|| Generation {
            copies: copies.clone(),
            deploy: None,
        });
        if let Some(newer) = copies.iter().find(|copy| !generation.copies.contains(copy)) {
            return Err(replanned(format!(
                "a newer copy {newer} of {service} appeared"
            )));
        }
        let mut waiting = false;
        for copy in copies {
            let protection = cancellable(cancellation, protection(client, service, &copy)).await?;
            within(deadline)?;
            match protection {
                Protection::Unnamed => {
                    cancellable(cancellation, client.remove_copy(machine_id, &copy)).await?;
                }
                Protection::Pointed(pointer) => {
                    let bound = generation
                        .deploy
                        .get_or_insert_with(|| pointer.deploy.clone());
                    if *bound != pointer.deploy {
                        return Err(replanned(format!(
                            "a newer deploy {} of {service} published its pointer",
                            pointer.deploy
                        )));
                    }
                    amend_pointer(
                        client,
                        service,
                        &pointer.deploy,
                        cancellation,
                        &Rotation::leaving([copy]),
                    )
                    .await?;
                    waiting = true;
                }
                Protection::Retiring => waiting = true,
            }
        }
        if !waiting {
            return Ok(());
        }
        let next = (tokio::time::Instant::now() + POINTED_RETIRE_POLL).min(deadline);
        cancellable(cancellation, async {
            tokio::time::sleep_until(next).await;
            Ok(())
        })
        .await?;
    }
}

struct Generation {
    copies: Vec<ContainerId>,
    deploy: Option<ployz_core::DeploymentLogId>,
}

fn replanned(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!("{message}; observe the Server again"),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

fn unavailable(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message,
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

async fn cancellable<T>(
    cancellation: &CancellationToken,
    work: impl Future<Output = Result<T, RpcError>>,
) -> Result<T, RpcError> {
    tokio::select! {
        result = work => result,
        () = cancellation.cancelled() => Err(cancelled()),
    }
}

fn cancelled() -> RpcError {
    unavailable("the drain was cancelled while it retired".into())
}

/// Retire `globals` on `server` only: each is stopped and removed there when fresh evidence
/// says the Server rules it out, and held when its eligibility is unknown. It starts
/// nothing, touches no other Global or Server, and stops at the next Global once
/// `cancellation` fires.
///
/// Returns each of `globals`, in order, with its retirement. The caller observes what
/// still runs.
pub(crate) async fn retire_globals<C: RetireClient>(
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
    let (live, names) = match client.live_services().await {
        Ok(observed) => observed,
        Err(error) => return everyone(crate::ui::row(&error)),
    };
    if !live.containers.all_targets_succeeded() {
        let server = |id: &MachineId| {
            names
                .iter()
                .find(|(named, _)| named == id)
                .map_or_else(|| id.to_string(), |(_, name)| name.to_string())
        };
        return everyone(format!(
            "cannot retire from partial Service observations: {}",
            crate::failure::partial_failure_details(&live.containers, server)
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
                    Some(slot) => match client.retire_slot(&server.id, &slot, cancellation).await {
                        Ok(()) => Retirement::Retired,
                        Err(error) => Retirement::NotRetired(crate::ui::row(&error)),
                    },
                },
            }
        };
        retirements.push((global.clone(), retirement));
    }
    retirements
}

#[cfg(test)]
#[path = "retirement_tests.rs"]
mod tests;
