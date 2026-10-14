//! The only writer of Serving Pointer rows: one compare-and-set per publish.
//!
//! Every read and publish goes to the lowest-id Machine that invites RPC. That Machine
//! answers only if it sees itself as the lowest such Machine, and otherwise refuses
//! naming the one it sees, so every compare-and-set runs on one Corrosion node unless a
//! partition leaves two Machines each seeing every lower id down.

use std::time::Duration;

use ployz_core::rpc::op;
use ployz_core::{
    ContainerObservation, DeploymentLogId, LiveServices, MachineTarget, NotPointerPublisher,
    PublishServingPointerRequest, QualifiedService, ReadServingPointerRequest, RpcError,
    ServingPointer, ServingPointerPublication,
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

/// What amending one Service's pointer came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Amendment {
    /// The Service has no pointer; nothing was written.
    NoPointer,
    /// The change was already in the pointer.
    Unchanged,
    /// Published after `retries` lost compare-and-sets.
    Published {
        retries: u32,
    },
    Outdated,
}

/// Amends `service`'s pointer with `change`, retrying lost compare-and-sets. With
/// `deploy`, a pointer under any other deploy is `Outdated` and left alone.
///
/// # Errors
///
/// Cancellation before the guarded write is sent is reported as such. Cancellation after
/// it was sent is an uncertain outcome: the write may have landed.
pub(crate) async fn amend_pointer<C: PointerClient>(
    client: &mut C,
    service: &QualifiedService,
    deploy: Option<&DeploymentLogId>,
    cancellation: &CancellationToken,
    change: impl Fn(&ServingPointer) -> Option<ServingPointer>,
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
        if deploy.is_some_and(|deploy| *deploy != current.deploy) {
            return Ok(Amendment::Outdated);
        }
        let Some(pointer) = change(&current) else {
            return Ok(Amendment::Unchanged);
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

/// `pointer` with `joining` added to its Containers and `leaving` moved to `retiring`,
/// or `None` when it already holds exactly that. With the complete Container listing in
/// `listed`, a retiring id it no longer holds is gone and leaves `retiring`.
pub(crate) fn rotated(
    pointer: &ServingPointer,
    joining: &[ployz_core::ContainerId],
    leaving: &[ployz_core::ContainerId],
    listed: Option<&[ployz_core::ContainerId]>,
) -> Option<ServingPointer> {
    let mut next = pointer.clone();
    if let Some(listed) = listed {
        next.retiring.retain(|id| listed.contains(id));
    }
    next.containers.retain(|id| !leaving.contains(id));
    for id in joining {
        if !next.containers.contains(id) {
            next.containers.push(*id);
        }
    }
    for id in leaving {
        if !next.retiring.contains(id) && !next.containers.contains(id) {
            next.retiring.push(*id);
        }
    }
    (next != *pointer).then_some(next)
}

#[cfg(test)]
#[path = "serving_publish_tests.rs"]
pub(crate) mod tests;
