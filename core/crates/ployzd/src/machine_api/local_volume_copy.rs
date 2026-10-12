//! SetVolumeCopy: the transition table and its Container half. ZFS runs on the plugin
//! through the routes in [`crate::volume_copy`].

use ployz_core::{
    ContainerId, ContainerKind, ContainerRuntimeObservation, CopyPlan, CopyRefusal, CopyTransition,
    DockerVolumeName, OpaquePayload, SetVolumeCopyRefused, SetVolumeCopyReply,
    SetVolumeCopyRequest, Start, VolumeCopy, WantedCopy, op,
};
use tonic::{Request, Response, Status};

use super::{MachineService, expect, local_error, respond};
use crate::{
    machine::LocalMachineError,
    volume_copy::{
        CUT_ROUTE, CopyRequest, CutRequest, DESTROY_ROUTE, FORGET_ROUTE, INSPECT_ROUTE, PULL_ROUTE,
        PullRequest, SNAPSHOT_ROUTE, SnapshotRequest, WRITER_ROUTE, WriterRequest,
    },
};

type Copy = Option<VolumeCopy>;

impl MachineService {
    pub(super) async fn set_copy(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::SetVolumeCopy>(request)?;
        let guard = std::sync::Arc::clone(&self.switch_mutation)
            .lock_owned()
            .await;
        if let Err(error) = self.require_joined() {
            return respond(error);
        }
        crate::faults::apply("SetVolumeCopy").await;
        // Detached with the switch guard, so a caller that hangs up mid-drain does not
        // free the next request to plan against a cut still running.
        let service = self.clone();
        let admitted = tokio::spawn(async move {
            let _guard = guard;
            service.set_copy_admitted(request).await
        })
        .await
        .map_err(LocalMachineError::from)
        .and_then(|copy| copy);
        match admitted {
            Ok(copy) => respond(SetVolumeCopyReply { copy }),
            Err(error) => local_error(error),
        }
    }

    async fn set_copy_admitted(
        &self,
        request: SetVolumeCopyRequest,
    ) -> Result<Copy, LocalMachineError> {
        let SetVolumeCopyRequest { name, expect, want } = request;
        let copy = self.inspect_copy(&name).await?;
        let start_running = match &want {
            WantedCopy::Writer {
                start: Some(start), ..
            } => self.start_container_id(start).await?.is_some(),
            WantedCopy::Writer { start: None, .. }
            | WantedCopy::Mirror { .. }
            | WantedCopy::Snapshot { .. }
            | WantedCopy::Forget => false,
        };
        let transition = match ployz_core::plan(copy.as_ref(), start_running, &expect, &want) {
            CopyPlan::AtWant => return Ok(copy),
            CopyPlan::Refuse(reason) => return Err(refused(reason, copy, &name)),
            CopyPlan::Transition(transition) => transition,
        };
        match (transition, want) {
            (CopyTransition::WriterToWriter, WantedCopy::Writer { at, epoch, start }) => {
                let copy = match copy {
                    Some(VolumeCopy::Root {
                        readonly,
                        epoch: held,
                        ..
                    }) if readonly || held != epoch => {
                        self.plugin_copy(WRITER_ROUTE, &WriterRequest { name, at, epoch })
                            .await?
                    }
                    copy => copy,
                };
                if let Some(start) = start {
                    self.run_start(&start).await?;
                }
                Ok(copy)
            }
            (CopyTransition::MirrorToWriter, WantedCopy::Writer { at, epoch, start }) => {
                let copy = self
                    .plugin_copy(WRITER_ROUTE, &WriterRequest { name, at, epoch })
                    .await?;
                crate::faults::kill_inside("SetVolumeCopy.MirrorToWriter");
                if let Some(start) = start {
                    self.run_start(&start).await?;
                }
                Ok(copy)
            }
            (CopyTransition::WriterToMirror, WantedCopy::Mirror { target, drain, .. }) => {
                let plugin = self.local.plugin().clone();
                let request = CutRequest {
                    name: name.clone(),
                    target,
                };
                let cut = self
                    .local
                    .cut_volume_copy(name.clone(), drain, move || async move {
                        plugin
                            .call::<Copy>(CUT_ROUTE, &request)
                            .await
                            .map(drop)
                            .map_err(LocalMachineError::StoragePreparation)
                    })
                    .await?;
                match cut {
                    Ok(()) => self.inspect_copy(&name).await,
                    Err(reason) => Err(refused(reason, self.inspect_copy(&name).await?, &name)),
                }
            }
            (CopyTransition::WriterToSnapshot, WantedCopy::Snapshot { name: snapshot }) => {
                self.plugin_copy(SNAPSHOT_ROUTE, &SnapshotRequest { name, snapshot })
                    .await
            }
            (CopyTransition::WriterToForget, WantedCopy::Forget) => {
                self.plugin_copy(FORGET_ROUTE, &CopyRequest { name }).await
            }
            (
                CopyTransition::MirrorToMirror | CopyTransition::NoneToMirror,
                WantedCopy::Mirror {
                    from: Some(from),
                    target,
                    ..
                },
            ) => {
                self.plugin_copy(PULL_ROUTE, &PullRequest { name, from, target })
                    .await
            }
            (CopyTransition::MirrorToForget, WantedCopy::Forget) => {
                self.plugin_copy(DESTROY_ROUTE, &CopyRequest { name }).await
            }
            (transition, want) => {
                unreachable!("the planner paired {transition:?} with {want:?}")
            }
        }
    }

    async fn inspect_copy(&self, name: &DockerVolumeName) -> Result<Copy, LocalMachineError> {
        self.plugin_copy(INSPECT_ROUTE, &CopyRequest { name: name.clone() })
            .await
    }

    async fn plugin_copy(
        &self,
        route: &str,
        request: &impl serde::Serialize,
    ) -> Result<Copy, LocalMachineError> {
        self.local
            .plugin()
            .call::<Copy>(route, request)
            .await
            .map_err(LocalMachineError::StoragePreparation)
    }

    /// The Container `start` names when Docker runs it now. A Kept Container that is
    /// gone is an error, not a stopped start: the caller's id is wrong.
    async fn start_container_id(
        &self,
        start: &Start,
    ) -> Result<Option<ContainerId>, LocalMachineError> {
        let containers = self.containers()?;
        let machine_id = self.local.record().id();
        let container_id = match start {
            Start::Kept { container_id } => *container_id,
            Start::Create {
                namespace,
                resolved_spec,
                creation_key,
            } => {
                let record = self.local.record();
                let machine = record
                    .machine()
                    .ok_or(LocalMachineError::NotParticipating)?;
                match containers
                    .find_creation(
                        machine,
                        namespace,
                        ContainerKind::ServiceContainer,
                        resolved_spec,
                        creation_key.as_str(),
                    )
                    .await?
                {
                    Some(created) => created.container_id,
                    None => return Ok(None),
                }
            }
        };
        let observed = containers
            .inspect_managed(&container_id, &machine_id)
            .await?;
        Ok(matches!(
            observed.runtime,
            ContainerRuntimeObservation::Running { .. }
        )
        .then_some(container_id))
    }

    /// Create the Container when `start` asks for one and it is absent, then start it.
    /// Both halves are idempotent, so a resend after either converges.
    async fn run_start(&self, start: &Start) -> Result<(), LocalMachineError> {
        let container_id = match start {
            Start::Kept { container_id } => *container_id,
            Start::Create {
                namespace,
                resolved_spec,
                creation_key,
            } => {
                self.local
                    .create_container(
                        ContainerKind::ServiceContainer,
                        namespace,
                        resolved_spec,
                        Some(creation_key.as_str().to_owned()),
                        None,
                        None,
                    )
                    .await?
                    .container_id
            }
        };
        self.local.start_container(container_id).await?;
        Ok(())
    }
}

fn refused(reason: CopyRefusal, copy: Copy, name: &DockerVolumeName) -> LocalMachineError {
    let message = match reason {
        CopyRefusal::Mismatch => format!("Volume {name} is not the copy the request expected"),
        CopyRefusal::NoTransition => format!("Volume {name} has no transition to the want"),
        CopyRefusal::Busy => format!("Volume {name} is still held by a running Container"),
        CopyRefusal::NoCapacity => format!("this Machine has no room for a copy of {name}"),
        CopyRefusal::Behind => format!("Volume {name} is not at the wanted snapshot"),
        CopyRefusal::Written => format!("Volume {name} was written after the wanted snapshot"),
        CopyRefusal::NoSource => format!("Volume {name} has no copy to pull from"),
    };
    LocalMachineError::StoragePreparation(SetVolumeCopyRefused { reason, copy }.rpc_error(message))
}
