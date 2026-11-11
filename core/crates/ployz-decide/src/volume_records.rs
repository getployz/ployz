//! One Machine's record of the Volume operations a Device approved: which are open, which
//! ended, who may pull what, and which recipients were withdrawn.
//!
//! An operation is admitted once inside its window and then runs on its record, so the
//! work a person started finishes after the window. An ended operation never runs again.
//! The daemon keeps these records in its approvals log and syncs each change before the
//! effect it guards; nothing here reads a clock.

use std::collections::BTreeMap;

use ployz_core::{
    CopyTransition, DockerVolumeName, MachineId, OperationId, SnapshotName,
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

/// What the source of a Move did after its cut. Written once.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Resumed,
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
    /// operation here and none of the replacement's.
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
            if record.state == OperationState::Ended {
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
            },
        );
        Ok(())
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
    pub fn decision(&self, operation: OperationId) -> Option<Decision> {
        self.operations
            .get(&operation)
            .and_then(|record| record.decision)
    }
}
