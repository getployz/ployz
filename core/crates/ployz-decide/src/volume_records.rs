//! One Machine's record of the Volume operations a Device approved: which are open, which
//! ended, who may pull what, and which recipients were withdrawn.
//!
//! An operation is admitted once inside its window and then runs on its record, so the
//! work a person started finishes after the window. An ended operation never runs again.
//! The daemon keeps these records in its approvals log and syncs each change before the
//! effect it guards; nothing here reads a clock.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerId, CopyTransition, DockerVolumeName, HandoffReceiptEnvelope, MachineId, OperationId,
    SnapshotGuid, SnapshotName, WriterEpoch,
    authority::{AuthorityRefusal, StepDigest, VolumeStep, VolumeWorkflow},
};
use serde::{Deserialize, Serialize};

/// What a grant lets its recipient pull.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    /// Every snapshot of the Volume: a Mirror's, until it is withdrawn.
    Standing,
    /// Only these snapshots, which this Machine took under the operation.
    Snapshots(Vec<SnapshotName>),
}

/// What the source of a Move did after its cut. Written once, and never changed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The source became the Writer again. No receipt follows.
    Resumed,
    /// The source gave the Volume up to its target, which may become the Writer under the
    /// receipt the source signs from this.
    Handed(Handoff),
}

/// A Move source's handoff: everything its receipt states beyond the operation itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Handoff {
    pub to: MachineId,
    /// The cut's snapshot.
    pub at: SnapshotGuid,
    pub snapshot: SnapshotName,
    /// The source's Writer epoch when it cut.
    pub from_epoch: WriterEpoch,
    /// The epoch the target takes.
    pub epoch: WriterEpoch,
}

/// A handoff this Machine recorded, with what its receipt names of the approval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Handed {
    pub operation: OperationId,
    pub digest: StepDigest,
    pub volume: DockerVolumeName,
    pub handoff: Handoff,
}

/// A recipient withdrawn from a Volume on this Machine, ranked against admissions by `at`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Withdrawal {
    pub volume: DockerVolumeName,
    pub recipient: MachineId,
    pub at: u64,
}

/// This Machine's Volume operations, grants and withdrawals. No record here is pruned.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeRecords {
    /// Raised by each admission and each withdrawal, so a withdrawal outranks exactly the
    /// operations admitted before it. It orders events on this Machine and reads no clock.
    sequence: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    operations: BTreeMap<OperationId, OperationRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grants: Vec<Grant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    withdrawals: Vec<Withdrawal>,
}

/// What a Move's source stopped at its cut, written before it stops anything: the snapshot
/// the cut takes, and the Containers it found running. Only these may start again under the
/// Move.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Cut {
    snapshot: SnapshotName,
    stopped: Vec<ContainerId>,
    /// The Writer epoch the root held when it was cut. A cut recorded before handoffs
    /// existed reads as 0.
    #[serde(default)]
    epoch: WriterEpoch,
}

/// What a Move's target became the Writer at, and the source's receipt it became it under.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Promotion {
    at: SnapshotGuid,
    epoch: WriterEpoch,
    receipt: HandoffReceiptEnvelope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct OperationRecord {
    /// The approval's body as signed, so another body under the same ID is refused.
    digest: StepDigest,
    volume: DockerVolumeName,
    workflow: VolumeWorkflow,
    admitted: u64,
    state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cut: Option<Cut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    promoted: Option<Promotion>,
    /// A pull ran under it here. A replacing Mirror destroys the copy it replaces only
    /// before its first pull, so once it pulled its approval destroys nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pulled: bool,
}

impl OperationRecord {
    /// Whether `step` on `machine` resends the step that ended this operation here, which
    /// returns what it recorded and changes nothing more: a Move source's resume after
    /// `Resumed`, its handoff after `Handed`, and its target's promotion and start after
    /// it promoted.
    fn resends_end(&self, machine: &MachineId, step: &VolumeStep) -> bool {
        let writer = matches!(
            step,
            VolumeStep::Copy(CopyTransition::WriterToWriter | CopyTransition::MirrorToWriter)
                | VolumeStep::Start
        );
        match (&self.workflow, &self.decision) {
            (VolumeWorkflow::Move { from, .. }, Some(Decision::Resumed)) if from == machine => {
                writer
            }
            (VolumeWorkflow::Move { from, .. }, Some(Decision::Handed(_))) if from == machine => {
                *step == VolumeStep::Handoff
            }
            (VolumeWorkflow::Move { to, .. }, _) if to == machine => {
                self.promoted.is_some() && writer
            }
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationState {
    Open,
    Ended,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Grant {
    operation: OperationId,
    volume: DockerVolumeName,
    recipient: MachineId,
    scope: GrantScope,
}

/// Whether `step` pulls the Volume into this Machine's copy.
fn pulls(step: &VolumeStep) -> bool {
    matches!(
        step,
        VolumeStep::Copy(CopyTransition::NoneToMirror | CopyTransition::MirrorToMirror)
    )
}

impl VolumeRecords {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Admit `step` of `operation` on `machine`'s copy of `volume`. An ended operation is
    /// refused for good. An open one passes on its record for any step its workflow permits
    /// here. Otherwise `verify` must pass, and the operation is recorded open. Admitting an
    /// operation that owns this Machine's copy ends the one that owned it before. Admitting
    /// a replacing Mirror's opening `MirrorToForget` also withdraws this Machine from the
    /// Volume, just before the replacement opens, so the withdrawal ends every other Mirror
    /// operation here and none of the replacement's. Its `MirrorToForget` passes on its
    /// record only until its first pull, so a standing approval never destroys the copy it
    /// made.
    ///
    /// # Errors
    /// Returns the [`AuthorityRefusal`] naming what the approval lacks, or
    /// [`AuthorityRefusal::Replayed`] for an operation already ended here.
    pub fn admit(
        &mut self,
        operation: OperationId,
        digest: StepDigest,
        machine: &MachineId,
        volume: &DockerVolumeName,
        step: &VolumeStep,
        verify: impl FnOnce() -> Result<VolumeWorkflow, AuthorityRefusal>,
    ) -> Result<(), AuthorityRefusal> {
        let refused = |field| Err(AuthorityRefusal::Mismatch { field });
        if let Some(record) = self.operations.get(&operation) {
            if record.state == OperationState::Ended
                && !(record.resends_end(machine, step)
                    && !self.superseded(operation, record.admitted, machine, volume))
            {
                return Err(AuthorityRefusal::Replayed { operation });
            }
            if record.digest != digest {
                return refused("an operation");
            }
            if record.volume != *volume {
                return refused("a Volume");
            }
            if !record.workflow.permits(machine, step) {
                return refused("a Volume step");
            }
            if record.cut.is_some() && *step == VolumeStep::Copy(CopyTransition::WriterToSnapshot) {
                return refused("a snapshot after the cut");
            }
            if record.pulled
                && *step == VolumeStep::Copy(CopyTransition::MirrorToForget)
                && matches!(record.workflow, VolumeWorkflow::Mirror { .. })
            {
                return refused("a Forget after the Mirror's pull");
            }
            if pulls(step) {
                self.operations
                    .get_mut(&operation)
                    .expect("read above")
                    .pulled = true;
            }
            return Ok(());
        }
        let workflow = verify()?;
        if workflow.owns(machine) {
            let owners: Vec<OperationId> = self
                .operations
                .iter()
                .filter(|(_, record)| {
                    record.state == OperationState::Open
                        && record.volume == *volume
                        && record.workflow.owns(machine)
                })
                .map(|(operation, _)| *operation)
                .collect();
            for owner in owners {
                self.end_owner(owner, machine);
            }
        }
        let replaces_here = matches!(
            &workflow,
            VolumeWorkflow::Mirror { mirror, replace: true, .. } if mirror == machine
        ) && *step == VolumeStep::Copy(CopyTransition::MirrorToForget);
        if replaces_here {
            self.withdraw(machine, volume, machine);
        }
        self.sequence += 1;
        self.operations.insert(
            operation,
            OperationRecord {
                digest,
                volume: volume.clone(),
                workflow,
                admitted: self.sequence,
                state: OperationState::Open,
                decision: None,
                cut: None,
                promoted: None,
                pulled: pulls(step),
            },
        );
        Ok(())
    }

    /// Whether this Machine admitted, after `operation` at `admitted`, another operation that
    /// owns its copy of `volume`. A resend of an ended operation's last step is then
    /// refused: a later owner holds the copy, whether or not it ended since.
    fn superseded(
        &self,
        operation: OperationId,
        admitted: u64,
        machine: &MachineId,
        volume: &DockerVolumeName,
    ) -> bool {
        self.operations.iter().any(|(other, record)| {
            *other != operation
                && record.admitted > admitted
                && record.volume == *volume
                && record.workflow.owns(machine)
        })
    }

    /// End an earlier owner of this Machine's copy. A Move this Machine is the source of,
    /// and that has no decision yet, records `Resumed`, so no handoff can follow.
    fn end_owner(&mut self, operation: OperationId, machine: &MachineId) {
        if let Some(record) = self.operations.get_mut(&operation)
            && record.workflow.moves_from(machine)
            && record.decision.is_none()
        {
            record.decision = Some(Decision::Resumed);
        }
        self.end(operation);
    }

    /// Record the cut of `operation`, a Move this Machine `machine` is the source of, before
    /// it stops anything: the snapshot `snapshot` it takes and the Containers in `running`.
    /// Grants the Move's target that snapshot. A resend of the same cut adds the Containers
    /// it finds running then, since the first may have stopped some before it crashed.
    ///
    /// # Errors
    /// Refuses an operation not open here, one that is not a Move from `machine`, a second
    /// cut at another snapshot, and a target withdrawn since the Move was admitted.
    pub fn cut(
        &mut self,
        operation: OperationId,
        machine: &MachineId,
        snapshot: &SnapshotName,
        epoch: WriterEpoch,
        running: &[ContainerId],
    ) -> Result<(), AuthorityRefusal> {
        let Some(record) = self.operations.get_mut(&operation) else {
            return Err(AuthorityRefusal::Mismatch {
                field: "an operation",
            });
        };
        if record.state == OperationState::Ended {
            return Err(AuthorityRefusal::Replayed { operation });
        }
        let VolumeWorkflow::Move { from, to, .. } = &record.workflow else {
            return Err(AuthorityRefusal::Mismatch { field: "a cut" });
        };
        if from != machine {
            return Err(AuthorityRefusal::Mismatch { field: "a cut" });
        }
        let to = *to;
        let cut = record.cut.get_or_insert_with(|| Cut {
            snapshot: snapshot.clone(),
            stopped: Vec::new(),
            epoch,
        });
        if cut.snapshot != *snapshot {
            return Err(AuthorityRefusal::Mismatch {
                field: "a second cut",
            });
        }
        for container in running {
            if !cut.stopped.contains(container) {
                cut.stopped.push(*container);
            }
        }
        self.grant(operation, to, GrantScope::Snapshots(vec![snapshot.clone()]))
    }

    /// Whether `operation`'s cut on this Machine stopped `container`.
    #[must_use]
    pub fn stopped_by_cut(&self, operation: OperationId, container: &ContainerId) -> bool {
        self.operations
            .get(&operation)
            .and_then(|record| record.cut.as_ref())
            .is_some_and(|cut| cut.stopped.contains(container))
    }

    /// Record that this Machine, the source of Move `operation`, resumes: the Move ends here
    /// with its grants, and only a resend of the resume runs under it again.
    ///
    /// # Errors
    /// Refuses a Move this Machine already handed off.
    pub fn resume(&mut self, operation: OperationId) -> Result<(), AuthorityRefusal> {
        if let Some(record) = self.operations.get_mut(&operation) {
            match record.decision {
                Some(Decision::Handed(_)) => return Err(AuthorityRefusal::Replayed { operation }),
                Some(Decision::Resumed) => {}
                None => record.decision = Some(Decision::Resumed),
            }
        }
        self.end(operation);
        Ok(())
    }

    /// Record that this Machine, the source of Move `operation`, hands its copy of the
    /// Volume to `to` at the cut's snapshot, which it holds as `at` and `snapshot`, and
    /// `to` takes `epoch`. The Move ends here: from then on its grant serves `to` the
    /// cut's snapshot and nothing else, and only a resend of this handoff runs under it,
    /// returning the same one.
    ///
    /// # Errors
    /// Refuses an operation not recorded here, one that is not a Move from `machine` to
    /// `to`, one with no cut or cut at another snapshot, an epoch not above the cut's, a
    /// Move this Machine resumed, and a second handoff that differs from the first.
    pub fn handoff(
        &mut self,
        operation: OperationId,
        machine: &MachineId,
        to: MachineId,
        (at, snapshot): (SnapshotGuid, &SnapshotName),
        epoch: WriterEpoch,
    ) -> Result<Handed, AuthorityRefusal> {
        let refused = |field| Err(AuthorityRefusal::Mismatch { field });
        let Some(record) = self.operations.get_mut(&operation) else {
            return refused("an operation");
        };
        if !matches!(&record.workflow, VolumeWorkflow::Move { from, to: target, .. } if from == machine && *target == to)
        {
            return refused("a handoff");
        }
        let (digest, volume) = (record.digest, record.volume.clone());
        let handoff = match &record.decision {
            Some(Decision::Resumed) => return Err(AuthorityRefusal::Replayed { operation }),
            Some(Decision::Handed(handed)) => {
                if handed.at != at || handed.epoch != epoch {
                    return refused("a second handoff");
                }
                handed.clone()
            }
            None => {
                if record.state == OperationState::Ended {
                    return Err(AuthorityRefusal::Replayed { operation });
                }
                let Some(cut) = &record.cut else {
                    return refused("a cut");
                };
                if cut.snapshot != *snapshot {
                    return refused("a snapshot");
                }
                if epoch <= cut.epoch {
                    return refused("an epoch");
                }
                let handoff = Handoff {
                    to,
                    at,
                    snapshot: cut.snapshot.clone(),
                    from_epoch: cut.epoch,
                    epoch,
                };
                record.decision = Some(Decision::Handed(handoff.clone()));
                record.state = OperationState::Ended;
                let cut = cut.snapshot.clone();
                self.grants
                    .retain(|grant| grant.operation != operation || grant.recipient == to);
                for grant in &mut self.grants {
                    if grant.operation == operation {
                        grant.scope = GrantScope::Snapshots(vec![cut.clone()]);
                    }
                }
                handoff
            }
        };
        Ok(Handed {
            operation,
            digest,
            volume,
            handoff,
        })
    }

    /// The handoff this Machine recorded of `volume` at the snapshot `at`, if any: the copy
    /// there was given up and never becomes the Writer again under a Move's word.
    #[must_use]
    pub fn handed_at(&self, volume: &DockerVolumeName, at: &SnapshotGuid) -> Option<Handed> {
        self.operations
            .iter()
            .find_map(|(operation, record)| match &record.decision {
                Some(Decision::Handed(handoff))
                    if record.volume == *volume && handoff.at == *at =>
                {
                    Some(Handed {
                        operation: *operation,
                        digest: record.digest,
                        volume: record.volume.clone(),
                        handoff: handoff.clone(),
                    })
                }
                _ => None,
            })
    }

    /// Record that this Machine, the target of Move `operation`, becomes the Writer at `at`
    /// with `epoch` under `receipt`, which the caller checked names them. Each Mirror the
    /// Move keeps following may pull every snapshot from here from then on, unless it was
    /// withdrawn; those grants outlive the Move. The Move ends here, and only a resend of
    /// this promotion and the approved start run under it again.
    ///
    /// # Errors
    /// Refuses an operation not recorded here, one that is not a Move to `machine`, one
    /// ended here before it promoted, and a promotion at another snapshot or epoch than
    /// the one recorded.
    pub fn promote(
        &mut self,
        operation: OperationId,
        machine: &MachineId,
        (at, epoch): (SnapshotGuid, WriterEpoch),
        receipt: HandoffReceiptEnvelope,
    ) -> Result<(), AuthorityRefusal> {
        let Some(record) = self.operations.get(&operation) else {
            return Err(AuthorityRefusal::Mismatch {
                field: "an operation",
            });
        };
        let VolumeWorkflow::Move { to, mirrors, .. } = &record.workflow else {
            return Err(AuthorityRefusal::Mismatch {
                field: "a promotion",
            });
        };
        if to != machine {
            return Err(AuthorityRefusal::Mismatch {
                field: "a promotion",
            });
        }
        if let Some(promoted) = &record.promoted {
            if promoted.at != at || promoted.epoch != epoch {
                return Err(AuthorityRefusal::Mismatch {
                    field: "a second promotion",
                });
            }
            return Ok(());
        }
        if record.state == OperationState::Ended {
            return Err(AuthorityRefusal::Replayed { operation });
        }
        for mirror in mirrors.clone() {
            match self.grant(operation, mirror, GrantScope::Standing) {
                Ok(()) | Err(AuthorityRefusal::Replayed { .. }) => {}
                Err(refusal) => return Err(refusal),
            }
        }
        let record = self.operations.get_mut(&operation).expect("checked above");
        record.promoted = Some(Promotion { at, epoch, receipt });
        record.state = OperationState::Ended;
        Ok(())
    }

    /// The workflow of `operation`, while it is recorded here.
    #[must_use]
    pub fn workflow(&self, operation: OperationId) -> Option<&VolumeWorkflow> {
        self.operations
            .get(&operation)
            .map(|record| &record.workflow)
    }

    /// End `operation` here, with its grants. It is refused from then on.
    pub fn end(&mut self, operation: OperationId) {
        if let Some(record) = self.operations.get_mut(&operation) {
            record.state = OperationState::Ended;
        }
        self.grants.retain(|grant| grant.operation != operation);
    }

    /// Let `recipient` pull `scope` of `operation`'s Volume from here while the operation is
    /// open. A grant merges into the one the operation already holds here.
    ///
    /// # Errors
    /// Refuses an operation not open here, a recipient its workflow does not send to, and
    /// one withdrawn since this Machine admitted the operation.
    pub fn grant(
        &mut self,
        operation: OperationId,
        recipient: MachineId,
        scope: GrantScope,
    ) -> Result<(), AuthorityRefusal> {
        let Some(record) = self.operations.get(&operation) else {
            return Err(AuthorityRefusal::Mismatch {
                field: "an operation",
            });
        };
        if record.state == OperationState::Ended {
            return Err(AuthorityRefusal::Replayed { operation });
        }
        let target =
            matches!(&record.workflow, VolumeWorkflow::Move { to, .. } if *to == recipient);
        let sends = record.workflow.mirrors_to(&recipient)
            || (target && matches!(scope, GrantScope::Snapshots(_)));
        if !sends {
            return Err(AuthorityRefusal::Mismatch {
                field: "a recipient",
            });
        }
        let outranked = self.withdrawals.iter().any(|withdrawal| {
            withdrawal.volume == record.volume
                && withdrawal.recipient == recipient
                && withdrawal.at > record.admitted
        });
        if outranked {
            return Err(AuthorityRefusal::Replayed { operation });
        }
        let volume = record.volume.clone();
        let held = self
            .grants
            .iter_mut()
            .find(|grant| grant.operation == operation && grant.recipient == recipient);
        match (held, scope) {
            (Some(held), GrantScope::Standing) => held.scope = GrantScope::Standing,
            (Some(held), GrantScope::Snapshots(added)) => {
                if let GrantScope::Snapshots(names) = &mut held.scope {
                    for name in added {
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                }
            }
            (None, scope) => self.grants.push(Grant {
                operation,
                volume,
                recipient,
                scope,
            }),
        }
        Ok(())
    }

    /// Whether a live grant lets `recipient` pull `snapshot` of `volume` from here, or,
    /// with no snapshot, any of it.
    #[must_use]
    pub fn live(
        &self,
        volume: &DockerVolumeName,
        recipient: &MachineId,
        snapshot: Option<&SnapshotName>,
    ) -> bool {
        self.grants.iter().any(|grant| {
            grant.volume == *volume
                && grant.recipient == *recipient
                && match (&grant.scope, snapshot) {
                    (GrantScope::Standing, _) | (GrantScope::Snapshots(_), None) => true,
                    (GrantScope::Snapshots(names), Some(snapshot)) => names.contains(snapshot),
                }
        })
    }

    /// Whether withdrawing `recipient` from `volume` would end anything this Machine holds:
    /// an open operation that mirrors the Volume to it, or a grant to it.
    #[must_use]
    pub fn sends(&self, volume: &DockerVolumeName, recipient: &MachineId) -> bool {
        self.operations.values().any(|record| {
            record.state == OperationState::Open
                && record.volume == *volume
                && record.workflow.mirrors_to(recipient)
        }) || self.live(volume, recipient, None)
    }

    /// Withdraw `recipient` from `volume` on `machine`. End every open operation here that
    /// mirrors it there: a Mirror's own, on either Machine, and a Move's following on the
    /// recipient itself. A Move between two other Machines goes on, but can no longer grant
    /// `recipient` anything. Every grant to it ends.
    pub fn withdraw(
        &mut self,
        machine: &MachineId,
        volume: &DockerVolumeName,
        recipient: &MachineId,
    ) {
        self.sequence += 1;
        self.withdrawals.push(Withdrawal {
            volume: volume.clone(),
            recipient: *recipient,
            at: self.sequence,
        });
        let mirrored: Vec<OperationId> = self
            .operations
            .iter()
            .filter(|(_, record)| {
                record.state == OperationState::Open
                    && record.volume == *volume
                    && record.workflow.mirrors_to(recipient)
                    && (matches!(record.workflow, VolumeWorkflow::Mirror { .. })
                        || machine == recipient)
            })
            .map(|(operation, _)| *operation)
            .collect();
        for operation in mirrored {
            self.end(operation);
        }
        self.grants
            .retain(|grant| !(grant.volume == *volume && grant.recipient == *recipient));
    }

    #[must_use]
    pub fn withdrawals(&self) -> &[Withdrawal] {
        &self.withdrawals
    }

    /// The decision a Move's source recorded for `operation`, if any.
    #[must_use]
    pub fn decision(&self, operation: OperationId) -> Option<&Decision> {
        self.operations
            .get(&operation)
            .and_then(|record| record.decision.as_ref())
    }

    /// Whether this Machine, a Move's target, promoted under `operation`.
    #[must_use]
    pub fn promoted(&self, operation: OperationId) -> bool {
        self.operations
            .get(&operation)
            .is_some_and(|record| record.promoted.is_some())
    }

    /// Whether `operation` ended here after this Machine admitted the body `digest` under it.
    #[must_use]
    pub fn ended_as(&self, operation: OperationId, digest: StepDigest) -> bool {
        self.operations
            .get(&operation)
            .is_some_and(|record| record.state == OperationState::Ended && record.digest == digest)
    }
}
