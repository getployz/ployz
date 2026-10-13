//! The only writer of Serving Pointer rows: one compare-and-set per publish.
//!
//! Every read and publish goes to the lowest-id Machine that invites RPC, so every
//! compare-and-set for a Cluster runs on one Corrosion node.

use std::time::Duration;

use ployz_core::rpc::op;
use ployz_core::{
    ContainerObservation, DeploymentLogId, LiveServices, MachineTarget,
    PublishServingPointerRequest, QualifiedService, ReadServingPointerRequest, RpcError,
    ServingPointer, ServingPointerPublication,
};

use crate::connect::{Client, ConnectError};

const PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);

/// Lost compare-and-sets an amendment retries before giving up.
pub(crate) const AMEND_RETRIES: u32 = 3;

/// What pointer publishing needs from a Cluster.
pub(crate) trait PointerClient {
    async fn read_pointer(
        &mut self,
        service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError>;
    async fn publish_pointer(
        &mut self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError>;
}

impl PointerClient for Client {
    async fn read_pointer(
        &mut self,
        service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        self.serving_pointer(service).await
    }

    async fn publish_pointer(
        &mut self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        self.publish_serving_pointer(request).await
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
        let target = self.publisher().await?;
        self.read::<op::ReadServingPointer>(
            ReadServingPointerRequest {
                service: service.clone(),
            },
            &target,
        )
        .await
        .map(|read| read.pointer)
    }

    /// Write `request.pointer` only if the row still holds `request.read`. Never retried.
    ///
    /// # Errors
    ///
    /// No Machine invites RPC, or the publish failed or timed out.
    pub async fn publish_serving_pointer(
        &mut self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        let target = self.publisher().await?;
        self.call_unretried::<op::PublishServingPointer>(request, Some(&target), PUBLISH_TIMEOUT)
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
    Published { retries: u32 },
    /// The pointer names a deploy other than the one the change was made for; nothing
    /// was written.
    Outdated,
}

/// Apply `change` to the Service's pointer, re-reading and retrying up to
/// [`AMEND_RETRIES`] times when another writer wins. `change` answers `None` when the
/// pointer already holds what it wants.
///
/// # Errors
///
/// A read or publish failed, or every retry lost.
pub(crate) async fn amend_pointer<C: PointerClient>(
    client: &mut C,
    service: &QualifiedService,
    deploy: Option<&DeploymentLogId>,
    change: impl Fn(&ServingPointer) -> Option<ServingPointer>,
) -> Result<Amendment, RpcError> {
    let Some(mut current) = client.read_pointer(service).await? else {
        return Ok(Amendment::NoPointer);
    };
    for retries in 0..=AMEND_RETRIES {
        if deploy.is_some_and(|deploy| *deploy != current.deploy) {
            return Ok(Amendment::Outdated);
        }
        let Some(pointer) = change(&current) else {
            return Ok(Amendment::Unchanged);
        };
        let request = PublishServingPointerRequest {
            service: service.clone(),
            read: Some(current),
            pointer,
        };
        match client.publish_pointer(request).await? {
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
/// or `None` when it already holds exactly that.
pub(crate) fn rotated(
    pointer: &ServingPointer,
    joining: &[ployz_core::ContainerId],
    leaving: &[ployz_core::ContainerId],
) -> Option<ServingPointer> {
    let mut next = pointer.clone();
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
