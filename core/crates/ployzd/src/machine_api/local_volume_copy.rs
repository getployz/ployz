//! SetVolumeCopy: the transition table and its Container half. ZFS runs on the plugin
//! through the routes in [`crate::volume_copy`].

use ployz_core::{
    ContainerKind, ContainerRuntimeObservation, CopyPlan, CopyRefusal, CopyTransition,
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

/// What Docker shows of a Writer's start before the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartState {
    Running,
    Stopped,
    Gone,
}

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
        let start = match &want {
            WantedCopy::Writer {
                start: Some(start), ..
            } => Some(self.start_state(start, &name).await?),
            WantedCopy::Writer { start: None, .. }
            | WantedCopy::Mirror { .. }
            | WantedCopy::Snapshot { .. }
            | WantedCopy::Forget => None,
        };
        let start_running = start == Some(StartState::Running);
        let transition = match ployz_core::plan(copy.as_ref(), start_running, &expect, &want) {
            CopyPlan::AtWant => return Ok(copy),
            CopyPlan::Refuse(reason) => return Err(refused(reason, copy, &name)),
            CopyPlan::Transition(_) if start == Some(StartState::Gone) => {
                return Err(refused(CopyRefusal::NoTransition, copy, &name));
            }
            CopyPlan::Transition(transition) => transition,
        };
        crate::faults::apply(fault_point(transition)).await;
        match (transition, want) {
            (CopyTransition::WriterToWriter, WantedCopy::Writer { at, epoch, start }) => {
                let copy = match copy {
                    Some(VolumeCopy::Root {
                        readonly,
                        epoch: held,
                        ..
                    }) if readonly || held != epoch => {
                        let request = WriterRequest {
                            name: name.clone(),
                            at,
                            epoch,
                        };
                        self.plugin_copy(WRITER_ROUTE, &request).await?
                    }
                    copy => copy,
                };
                // Another holder already runs: starting this one too would run two. Read
                // again here, since a holder can exit or go while the Writer route runs.
                if let Some(start) = start {
                    match self.start_state(&start, &name).await? {
                        StartState::Running => {}
                        StartState::Gone => {
                            return Err(refused(CopyRefusal::NoTransition, copy, &name));
                        }
                        StartState::Stopped => self.run_start(&start).await?,
                    }
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
            (CopyTransition::MirrorToForget | CopyTransition::SeedToForget, WantedCopy::Forget) => {
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

    /// Whether the Writer's start already runs. It runs when Docker runs the Container
    /// `start` names, or any other holder of the Volume: a Service that moved on to a
    /// new Container after the start landed has the start's effect already. A Kept
    /// Container Docker no longer has is gone, and nothing can start it again.
    async fn start_state(
        &self,
        start: &Start,
        name: &DockerVolumeName,
    ) -> Result<StartState, LocalMachineError> {
        let containers = self.containers()?;
        let machine_id = self.local.record().id();
        let container_id = match start {
            Start::Kept { container_id } => Some(*container_id),
            Start::Create {
                namespace,
                resolved_spec,
                creation_key,
            } => {
                let record = self.local.record();
                let machine = record
                    .machine()
                    .ok_or(LocalMachineError::NotParticipating)?;
                containers
                    .find_creation(
                        machine,
                        namespace,
                        ContainerKind::ServiceContainer,
                        resolved_spec,
                        creation_key.as_str(),
                    )
                    .await?
                    .map(|created| created.container_id)
            }
        };
        let observed = match container_id {
            Some(container_id) => {
                match containers.inspect_managed(&container_id, &machine_id).await {
                    Ok(observed) => Some(observed),
                    Err(crate::docker::Error::ContainerNotFound(_)) => None,
                    Err(error) => return Err(error.into()),
                }
            }
            None => None,
        };
        if observed.as_ref().is_some_and(|observed| {
            matches!(
                observed.runtime,
                ContainerRuntimeObservation::Running { .. }
            )
        }) || self.local.volume_holder_running(name).await?
        {
            return Ok(StartState::Running);
        }
        Ok(match (start, observed) {
            (Start::Kept { .. }, None) => StartState::Gone,
            _ => StartState::Stopped,
        })
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

/// A delay names one cell, so a run can slow the promote without slowing every pull poll.
fn fault_point(transition: CopyTransition) -> &'static str {
    match transition {
        CopyTransition::WriterToWriter => "SetVolumeCopy.WriterToWriter",
        CopyTransition::WriterToMirror => "SetVolumeCopy.WriterToMirror",
        CopyTransition::WriterToSnapshot => "SetVolumeCopy.WriterToSnapshot",
        CopyTransition::WriterToForget => "SetVolumeCopy.WriterToForget",
        CopyTransition::SeedToForget => "SetVolumeCopy.SeedToForget",
        CopyTransition::MirrorToWriter => "SetVolumeCopy.MirrorToWriter",
        CopyTransition::MirrorToMirror => "SetVolumeCopy.MirrorToMirror",
        CopyTransition::MirrorToForget => "SetVolumeCopy.MirrorToForget",
        CopyTransition::NoneToMirror => "SetVolumeCopy.NoneToMirror",
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
