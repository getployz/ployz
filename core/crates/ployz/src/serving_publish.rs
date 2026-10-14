//! The only writer of Serving Pointer rows: one compare-and-set per publish.
//!
//! Every read and publish goes to the lowest-id Machine that invites RPC. That Machine
//! answers only if it sees itself as the lowest such Machine, and otherwise refuses
//! naming the one it sees, so every compare-and-set runs on one Corrosion node unless a
//! partition leaves two Machines each seeing every lower id down.

use std::time::Duration;

use ployz_core::rpc::op;
use ployz_core::{
    ContainerId, ContainerObservation, DeploymentLogId, LiveServices, MachineTarget,
    NotPointerPublisher, PublishServingPointerRequest, QualifiedService, ReadServingPointerRequest,
    RpcError, ServingPointer, ServingPointerPublication,
};

use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError};

const PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);

/// Lost compare-and-sets an amendment retries before giving up.
pub(crate) const AMEND_RETRIES: u32 = 3;

/// What pointer publishing needs from a Cluster.
pub(crate) trait PointerClient {
    async fn pointer_publisher(&mut self) -> Result<MachineTarget, RpcError>;
    async fn read_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError>;
    async fn publish_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError>;

    async fn read_pointer(
        &mut self,
        service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        let mut at = self.pointer_publisher().await?;
        self.read_pointer_at(&mut at, service).await
    }
}

impl PointerClient for Client {
    async fn pointer_publisher(&mut self) -> Result<MachineTarget, RpcError> {
        self.publisher().await
    }

    async fn read_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        let request = ReadServingPointerRequest {
            service: service.clone(),
        };
        let read = match self
            .read::<op::ReadServingPointer>(request.clone(), at)
            .await
        {
            Err(error) => match NotPointerPublisher::from_error(&error) {
                Some(refusal) => {
                    *at = MachineTarget::from(&refusal.publisher);
                    self.read::<op::ReadServingPointer>(request, at).await
                }
                None => Err(error),
            },
            read => read,
        };
        read.map(|read| read.pointer)
    }

    async fn publish_pointer_at(
        &mut self,
        at: &mut MachineTarget,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        match self.publish_once(request.clone(), at).await {
            Err(error) => match NotPointerPublisher::from_error(&error) {
                Some(refusal) => {
                    *at = MachineTarget::from(&refusal.publisher);
                    self.publish_once(request, at).await
                }
                None => Err(error),
            },
            publication => publication,
        }
    }
}

impl Client {
    /// The Service's Serving Pointer, as the publishing Machine holds it.
    ///
    /// # Errors
    ///
    /// No Machine invites RPC, or the read failed.
    pub async fn serving_pointer(
        &mut self,
        service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        self.read_pointer(service).await
    }

    /// Write `request.pointer` only if the row still holds `request.read`. A refusal
    /// is retried once at the Machine it names; nothing else is retried.
    ///
    /// # Errors
    ///
    /// No Machine invites RPC, or the publish failed or timed out.
    pub async fn publish_serving_pointer(
        &mut self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        let mut at = self.publisher().await?;
        self.publish_pointer_at(&mut at, request).await
    }

    async fn publish_once(
        &mut self,
        request: PublishServingPointerRequest,
        at: &MachineTarget,
    ) -> Result<ServingPointerPublication, RpcError> {
        self.call_unretried::<op::PublishServingPointer>(request, Some(at), PUBLISH_TIMEOUT)
            .await
            .map_err(RpcError::from)
    }

    async fn publisher(&mut self) -> Result<MachineTarget, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        machines
            .iter()
            .filter(|machine| machine.invites_rpc())
            .map(|machine| &machine.machine.id)
            .min()
            .map(MachineTarget::from)
            .ok_or_else(|| {
                RpcError::from(ConnectError::from(tonic::Status::unavailable(
                    "no Machine accepts Serving Pointer publishes",
                )))
            })
    }
}

/// The pointer-membership contract. A copy id stands in one of four states relative to a
/// Service's Serving Pointer row, and every change between them runs through one path:
/// `Rotation` applied inside `amend_pointer` against the row the compare-and-set reads, or
/// `Protection` read immediately before a stop or removal.
///
/// | state    | the row                                                           |
/// |----------|-------------------------------------------------------------------|
/// | unnamed  | names it in neither `containers` nor `retiring`                   |
/// | pointed  | names it in `containers`: Machines route to it                    |
/// | retiring | names it in `retiring`: Machines stop routing, then ployzd stops it |
/// | gone     | names it or not; no listing shows it                              |
///
/// | transition          | evidence, read immediately before the step                        | who |
/// |---------------------|-------------------------------------------------------------------|-----|
/// | unnamed -> pointed  | the writer is bound to the row's deploy, and the copy's spec came from a source copy the row names in `containers` or whose deploy label is the row's deploy | catch-up, drain |
/// | pointed -> retiring | the row names the id in `containers`; other leaving ids are ignored | catch-up, drain, retirement |
/// | retiring -> unnamed | the row read before a complete listing named it in `retiring` and that listing lacks it | catch-up |
/// | retiring -> gone    | every up Machine acknowledged the row                             | ployzd retire |
/// | unnamed -> gone     | a read immediately before each stop or removal shows no row naming it | drain, retirement, catch-up |
/// | pointed -> gone     | never directly: pointed -> retiring first                         |     |
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Rotation {
    pub(crate) joining: Vec<Join>,
    pub(crate) leaving: Vec<ContainerId>,
    /// Retiring ids a complete listing proved absent; see `absent_retiring`.
    pub(crate) pruning: Vec<ContainerId>,
}

/// A copy entering `containers`, with the copy whose spec it carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Join {
    pub(crate) copy: ContainerId,
    pub(crate) provenance: Provenance,
}

/// Where a copy's spec came from: the copy it was read from and that copy's deploy label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Provenance {
    pub(crate) source: ContainerId,
    /// The source's `DEPLOYMENT_ID_LABEL`, when the caller observed it.
    pub(crate) source_deploy: Option<DeploymentLogId>,
}

impl Provenance {
    /// Whether `pointer`'s deploy owns the spec read from `source`.
    pub(crate) fn proven_by(&self, pointer: &ServingPointer) -> bool {
        pointer.containers.contains(&self.source)
            || self.source_deploy.as_ref() == Some(&pointer.deploy)
    }
}

impl Join {
    fn proven_by(&self, pointer: &ServingPointer) -> bool {
        self.provenance.proven_by(pointer)
    }
}

impl Rotation {
    pub(crate) fn leaving(ids: impl IntoIterator<Item = ContainerId>) -> Self {
        Self {
            leaving: ids.into_iter().collect(),
            ..Self::default()
        }
    }

    fn apply(&self, current: &ServingPointer) -> Applied {
        let mut next = current.clone();
        next.retiring.retain(|id| !self.pruning.contains(id));
        for join in &self.joining {
            if next.containers.contains(&join.copy) {
                continue;
            }
            if !join.proven_by(current) {
                return Applied::Unproven;
            }
            next.containers.push(join.copy);
        }
        for id in &self.leaving {
            if !current.containers.contains(id) {
                continue;
            }
            next.containers.retain(|named| named != id);
            if !next.retiring.contains(id) {
                next.retiring.push(*id);
            }
        }
        if next == *current {
            Applied::Unchanged
        } else {
            Applied::Next(next)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Applied {
    Unchanged,
    Next(ServingPointer),
    /// `current` carries no evidence for a joining copy.
    Unproven,
}

/// The ids `read.retiring` names that `listed`, a complete listing taken after `read`,
/// lacks. Only those leave `retiring` without passing through ployzd's retire.
pub(crate) fn absent_retiring(read: &ServingPointer, listed: &[ContainerId]) -> Vec<ContainerId> {
    read.retiring
        .iter()
        .filter(|id| !listed.contains(id))
        .copied()
        .collect()
}

/// What a read immediately before a stop or removal says about one copy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Protection {
    /// No row names it; it may be stopped or removed.
    Unnamed,
    /// The row names it in `containers`; it leaves only through `Rotation::leaving`.
    Pointed(ServingPointer),
    /// The row names it in `retiring`; ployzd stops it after every Machine acknowledges.
    Retiring,
}

pub(crate) async fn protection<C: PointerClient>(
    client: &mut C,
    service: &QualifiedService,
    copy: &ContainerId,
) -> Result<Protection, RpcError> {
    Ok(match client.read_pointer(service).await? {
        Some(pointer) if pointer.containers.contains(copy) => Protection::Pointed(pointer),
        Some(pointer) if pointer.retiring.contains(copy) => Protection::Retiring,
        _ => Protection::Unnamed,
    })
}

/// What amending one Service's pointer came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Amendment {
    /// The Service has no pointer; nothing was written.
    NoPointer,
    /// The change was already in the pointer.
    Unchanged,
    /// Published after `retries` lost compare-and-sets.
    Published { retries: u32 },
    /// The row belongs to another deploy than the bound one, or carries no evidence that
    /// its deploy owns a joining copy's spec. Nothing was written.
    Outdated,
}

pub(crate) async fn amend_pointer<C: PointerClient>(
    client: &mut C,
    service: &QualifiedService,
    deploy: &DeploymentLogId,
    cancellation: &CancellationToken,
    rotation: &Rotation,
) -> Result<Amendment, RpcError> {
    let before_write = || {
        unavailable(format!(
            "cancelled before the Serving Pointer of {service} was written"
        ))
    };
    let mut at = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(before_write()),
        at = client.pointer_publisher() => at?,
    };
    let read = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(before_write()),
        read = client.read_pointer_at(&mut at, service) => read?,
    };
    let Some(mut current) = read else {
        return Ok(Amendment::NoPointer);
    };
    for retries in 0..=AMEND_RETRIES {
        if *deploy != current.deploy {
            return Ok(Amendment::Outdated);
        }
        let pointer = match rotation.apply(&current) {
            Applied::Next(pointer) => pointer,
            Applied::Unchanged => return Ok(Amendment::Unchanged),
            Applied::Unproven => return Ok(Amendment::Outdated),
        };
        if cancellation.is_cancelled() {
            return Err(before_write());
        }
        let request = PublishServingPointerRequest {
            service: service.clone(),
            read: Some(current),
            pointer,
        };
        let publication = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(unavailable(format!(
                "cancelled after sending the Serving Pointer of {service}; it may have been written"
            ))),
            publication = client.publish_pointer_at(&mut at, request) => publication?,
        };
        match publication {
            ServingPointerPublication::Published => return Ok(Amendment::Published { retries }),
            ServingPointerPublication::Superseded { current: None } => {
                return Ok(Amendment::NoPointer);
            }
            ServingPointerPublication::Superseded {
                current: Some(newer),
            } => current = newer,
        }
    }
    Err(RpcError {
        code: ployz_core::RpcErrorCode::Conflict,
        message: format!(
            "lost the Serving Pointer of {service} to other writers {} times",
            AMEND_RETRIES + 1
        ),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    })
}

fn unavailable(message: String) -> RpcError {
    RpcError {
        code: ployz_core::RpcErrorCode::Unavailable,
        message,
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

/// The Service's Containers in `live`, or `None` when some Machine did not answer, since a
/// copy missing from a partial listing may still be serving.
pub(crate) fn complete_members<E>(
    live: &LiveServices<E>,
    service: &QualifiedService,
) -> Option<Vec<ContainerObservation>> {
    if !live.containers.all_targets_succeeded() {
        return None;
    }
    Some(
        live.services()
            .into_iter()
            .find(|observed| observed.identity == *service)
            .map(|observed| {
                observed
                    .members()
                    .map(|container| container.as_observation().clone())
                    .collect()
            })
            .unwrap_or_default(),
    )
}

#[cfg(test)]
#[path = "serving_publish_tests.rs"]
pub(crate) mod tests;
