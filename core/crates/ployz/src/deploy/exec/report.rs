//! How a Deploy names a copy it leaves running, and why.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerId, ContainerRefusal, DeployWarning, ExecutionError, MachineAction, MachineId,
    MachineName, QualifiedService, RpcError, RpcErrorCode,
};

use super::machine_error;
use crate::deploy::cleanup::{self, RotationFailure};
use crate::deploy::progress::Progress;

#[derive(Clone)]
pub(super) enum Left {
    NotConfirmed(cleanup::Unconfirmed),
    Interrupted,
    /// The entry Machine could not read the routes, so no copy was seen unlisted.
    Unread(RpcError),
    /// The owner's view never listed a new copy, so it would refuse the Out.
    NotListed,
    Refused(ContainerRefusal),
    /// A call to the copy's owner failed.
    Failed(RpcError),
    /// This run Held the copy and could not bring it back In, for this reason.
    NotRestored(Box<Left>, String),
    OlderNotRemoved(Box<Left>),
    DeployFailed,
    /// A removal could not reach the copy's Machine. It stays until the next full
    /// Deploy prunes it, and its store row keeps the live copies' drain unconfirmed.
    Unreached,
}

impl From<RotationFailure> for Left {
    fn from(failure: RotationFailure) -> Self {
        match failure {
            RotationFailure::Refused(refusal) => Left::Refused(refusal),
            RotationFailure::NotConfirmed(unconfirmed) => Left::NotConfirmed(unconfirmed),
            RotationFailure::Cancelled => Left::Interrupted,
        }
    }
}

impl From<cleanup::Rotated> for Option<Left> {
    fn from(rotated: cleanup::Rotated) -> Self {
        rotated.failure().err().map(Left::from)
    }
}

pub(super) fn left_by(error: ExecutionError) -> Left {
    match error {
        ExecutionError::Machine { error, .. } => Left::Failed(error),
        error @ (ExecutionError::Health { .. }
        | ExecutionError::DependencyHealth { .. }
        | ExecutionError::Hook { .. }
        | ExecutionError::NewerServing { .. }
        | ExecutionError::Cancelled) => {
            Left::Failed(RpcError::caused(RpcErrorCode::Internal, &error))
        }
    }
}

/// The Deploy's report of copies it leaves running, with Machines by name.
pub(super) struct Report<'a> {
    names: &'a BTreeMap<MachineId, MachineName>,
    /// Machines the plan's listing found unanswered over a path dropping large packets.
    dropped_large_packets: &'a BTreeSet<MachineId>,
    /// Copies on unobserved Machines a removal already left to the next full Deploy,
    /// so a Service losing several live copies names each one once.
    unreached: std::sync::Mutex<BTreeSet<ContainerId>>,
}

impl<'a> Report<'a> {
    pub(super) fn new(
        names: &'a BTreeMap<MachineId, MachineName>,
        dropped_large_packets: &'a BTreeSet<MachineId>,
    ) -> Self {
        Self {
            names,
            dropped_large_packets,
            unreached: std::sync::Mutex::new(BTreeSet::new()),
        }
    }

    /// What a rotation that did not finish leaves its copy as, and the Deploy's error
    /// for `action`.
    pub(super) fn failed(
        &self,
        failure: RotationFailure,
        action: MachineAction,
    ) -> (Left, ExecutionError) {
        let error = match &failure {
            RotationFailure::Refused(refusal) => {
                machine_error(action, refusal.clone().into_error())
            }
            RotationFailure::NotConfirmed(unconfirmed) => machine_error(
                action,
                RpcError::unavailable(self.not_confirmed(unconfirmed)),
            ),
            RotationFailure::Cancelled => ExecutionError::Cancelled,
        };
        (Left::from(failure), error)
    }

    pub(super) fn not_confirmed(&self, unconfirmed: &cleanup::Unconfirmed) -> String {
        let causes: String = unconfirmed
            .machines
            .iter()
            .filter(|id| self.dropped_large_packets.contains(id))
            .map(|id| crate::deploy::dropped_note(self.machine_name(id)))
            .collect();
        format!(
            "{}{causes}",
            unconfirmed.left_reason(|id| self.machine_name(id))
        )
    }

    fn machine_name(&self, id: &MachineId) -> String {
        self.names
            .get(id)
            .map_or_else(|| id.to_string(), |name| name.to_string())
    }

    /// A copy this run couldn't remove that never went In: its own new one, or one an
    /// ended run left. No proxy routes it once its Machine is back, and the next
    /// Deploy removes it.
    pub(super) fn leave_unrouted(
        &self,
        progress: &Progress,
        service: &QualifiedService,
        machine_id: MachineId,
        container_id: ContainerId,
        error: RpcError,
    ) {
        let (reason, _) = self.failed_on(machine_id, error);
        self.name_left(progress, service, machine_id, container_id, reason);
    }

    /// A copy this run couldn't remove, for `reason`, which keeps running.
    pub(super) fn name_left(
        &self,
        progress: &Progress,
        service: &QualifiedService,
        machine_id: MachineId,
        container_id: ContainerId,
        reason: String,
    ) {
        progress.warn(DeployWarning::LeftRunning {
            service: service.clone(),
            container_id,
            machine: self.machine_name(&machine_id),
            reason,
        });
    }

    /// Names each copy a removal leaves on a Machine it could not reach, once.
    pub(super) fn leave_unreached(&self, progress: &Progress, copies: &[cleanup::Copy]) {
        let mut named = self.unreached.lock().expect("unreached copies lock");
        for copy in copies {
            if named.insert(copy.container_id) {
                self.leave_running(progress, copy, Left::Unreached);
            }
        }
    }

    /// Name a withdrawn copy whose removal wasn't confirmed, based on its last
    /// observed runtime state.
    pub(super) fn unfinished_removal(
        &self,
        progress: &Progress,
        copy: &cleanup::Copy,
        running: bool,
        left: Left,
    ) {
        if running {
            self.leave_running(progress, copy, left);
        } else {
            let (reason, _) = self.explain(copy, left);
            progress.warn(DeployWarning::CleanupUnconfirmed {
                service: copy.service.clone(),
                container_id: copy.container_id,
                machine: self.machine_name(&copy.machine_id),
                reason,
            });
        }
    }

    /// A Machine that was cut off or powered off can route the copy again once
    /// it is back, so the report names each one.
    pub(super) fn leave_running(&self, progress: &Progress, copy: &cleanup::Copy, left: Left) {
        let (reason, away) = self.explain(copy, left);
        progress.warn(DeployWarning::LeftRunning {
            service: copy.service.clone(),
            container_id: copy.container_id,
            machine: self.machine_name(&copy.machine_id),
            reason,
        });
        for machine in away {
            progress.warn(DeployWarning::MixedAfterReturn {
                machine: self.machine_name(&machine),
            });
        }
    }

    fn explain(&self, copy: &cleanup::Copy, left: Left) -> (String, Vec<MachineId>) {
        match left {
            Left::NotConfirmed(unconfirmed) => {
                (self.not_confirmed(&unconfirmed), unconfirmed.unanswered)
            }
            Left::Interrupted => ("interrupted".into(), Vec::new()),
            Left::Unread(error) => (
                format!(
                    "the routes could not be read through the Machine this Deploy connects to: {}",
                    error.message
                ),
                Vec::new(),
            ),
            Left::DeployFailed => ("the Deploy failed".into(), Vec::new()),
            Left::Unreached => (
                format!(
                    "{} didn't answer, the next full deploy removes its copy once it is back",
                    self.machine_name(&copy.machine_id)
                ),
                Vec::new(),
            ),
            Left::NotListed => (
                format!(
                    "{} did not see the new copies listed",
                    self.machine_name(&copy.machine_id)
                ),
                Vec::new(),
            ),
            Left::Refused(refusal) => (refusal.message, Vec::new()),
            Left::Failed(error) => self.failed_on(copy.machine_id, error),
            Left::NotRestored(left, why) => {
                let (reason, away) = self.explain(copy, *left);
                (
                    format!("{reason}, and it could not go back In: {why}"),
                    away,
                )
            }
            Left::OlderNotRemoved(left) => {
                let (reason, away) = self.explain(copy, *left);
                let newer = ExecutionError::NewerServing {
                    service: copy.service.clone(),
                };
                (format!("{newer}, and removing it failed: {reason}"), away)
            }
        }
    }

    /// A call the client saw go unanswered names its target as away. So does an
    /// unavailable call to the copy's own owner, which covers this client's own
    /// deadline. Any other error says nothing about which Machine is cut off.
    fn failed_on(&self, machine_id: MachineId, error: RpcError) -> (String, Vec<MachineId>) {
        match cleanup::did_not_answer(&self.machine_name(&machine_id), &error) {
            Some(reason) => (reason, vec![machine_id]),
            None => (error.message, Vec::new()),
        }
    }
}
