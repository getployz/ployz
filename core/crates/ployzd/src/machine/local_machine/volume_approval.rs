//! The Volume verbs an enforcing Machine refuses from an Operator caller, and this
//! Machine's own record of the Volume operations a Device approved: which are open, which
//! ended, who may pull what, and which recipients were withdrawn.
//!
//! An operation is admitted once inside its window and then runs on its record, so the
//! work a person started finishes after the window. An ended operation never runs again.
//! Each record is synced before the effect it guards, and none reads a clock again.

use std::collections::BTreeMap;

use ployz_core::{
    DockerVolumeName, MachineId, OperationId, SEED_REFUSED_BY_ENFORCEMENT, SEED_VOLUME_CAPABILITY,
    SET_VOLUME_COPY_CAPABILITY, Sealing, SnapshotName, VOLUME_START_REFUSED_BY_ENFORCEMENT,
    authority::{
        AuthorityRefusal, Signed, StepDigest, Timestamp, VolumeApproval, VolumeFacts, VolumeStep,
        VolumeWorkflow, volume,
    },
};
use serde::{Deserialize, Serialize};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Admit `step` on `volume` under `approval`. An operation this Machine ended is
    /// refused for good. An operation open here passes on its record, at any time, for any
    /// step its workflow permits this Machine. Otherwise the approval must verify inside its
    /// window, and the operation is recorded open, synced, before this returns. Admitting
    /// an operation that owns this Machine's copy ends the one that owned it before.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] naming what the approval lacks, an operation
    /// already ended here as [`AuthorityRefusal::Replayed`], and [`Error::ApprovalRecord`]
    /// when the record cannot be read or written.
    pub fn admit_volume_step(
        &self,
        approval: &Signed<VolumeApproval>,
        volume: &DockerVolumeName,
        step: &VolumeStep,
    ) -> Result<OperationId, Error> {
        let record = self.record();
        let machine = record.id();
        let trusted = record.trusted_keys();
        let cluster = match self.held_sealing(record.phase()) {
            Some(Sealing::Held { public_key }) => Some(public_key),
            _ => None,
        };
        let operation = approval.open().map_err(Error::VolumeStepRefused)?.operation;
        let digest = StepDigest::of(&approval.body);
        self.approvals
            .change_volumes(Timestamp::now(), |records, now| {
                records.admit(operation, digest, &machine, volume, step, || {
                    volume::verify(
                        approval,
                        &VolumeFacts {
                            cluster,
                            machine: &machine,
                            trusted: &trusted,
                            volume,
                            step,
                            now,
                        },
                    )
                    .map(|approved| approved.body.workflow)
                })
            })
            .inspect(|()| tracing::info!(%operation, %volume, ?step, "Volume step admitted"))?;
        Ok(operation)
    }

    /// End `operation` on this Machine, with its grants. It is refused from then on.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub fn end_volume_operation(&self, operation: OperationId) -> Result<(), Error> {
        self.approvals
            .change_volumes(Timestamp::now(), |records, _| {
                records.end(operation);
                Ok(())
            })
    }

    /// Let `recipient` pull `scope` of `operation`'s Volume from this Machine while the
    /// operation is open. A grant merges into the one the operation already holds there.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] for an operation not open here, a recipient
    /// its workflow does not send to, or one withdrawn since this Machine admitted it, and
    /// [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub fn grant_volume(
        &self,
        operation: OperationId,
        recipient: MachineId,
        scope: GrantScope,
    ) -> Result<(), Error> {
        self.approvals
            .change_volumes(Timestamp::now(), |records, _| {
                records.grant(operation, recipient, scope)
            })
    }

    /// Whether a live grant lets `recipient` pull `snapshot` of `volume` from here.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read.
    pub fn volume_grant_live(
        &self,
        volume: &DockerVolumeName,
        recipient: &MachineId,
        snapshot: &SnapshotName,
    ) -> Result<bool, Error> {
        self.approvals
            .read_volumes(|records| records.live(volume, recipient, snapshot))
    }

    /// Withdraw `recipient` from `volume` here, before anything is destroyed: every Mirror
    /// operation this Machine admitted for it ends, and so does every grant to it. It needs
    /// no approval, because it only narrows what a driver can do. An approval this Machine
    /// never admitted is still new work while its window lasts.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub fn withdraw_volume(
        &self,
        volume: &DockerVolumeName,
        recipient: &MachineId,
    ) -> Result<(), Error> {
        let machine = self.record().id();
        self.approvals
            .change_volumes(Timestamp::now(), |records, _| {
                records.withdraw(&machine, volume, recipient);
                Ok(())
            })
    }
    /// Under Device enforcement an Operator's `SetVolumeCopy` that starts the Writer's
    /// Container is refused before the switch lock. A Create runs a spec the caller
    /// chose. A Kept start names a stopped Container that nothing on this Machine ties
    /// to the Volume or to a stop a person approved.
    ///
    /// # Errors
    /// Returns [`Error::VolumeRefused`] naming `SetVolumeCopy` under enforcement.
    pub(crate) fn admit_operator_volume_start(&self) -> Result<(), Error> {
        self.admit_operator_volume(SET_VOLUME_COPY_CAPABILITY)
    }

    /// Under Device enforcement an Operator's `SeedVolume` is refused before the switch
    /// lock. It writes a copy's data into a Volume name the caller picks.
    ///
    /// # Errors
    /// Returns [`Error::VolumeRefused`] naming `SeedVolume` under enforcement.
    pub(crate) fn admit_operator_seed(&self) -> Result<(), Error> {
        self.admit_operator_volume(SEED_VOLUME_CAPABILITY)
    }

    fn admit_operator_volume(&self, capability: &'static str) -> Result<(), Error> {
        if self.record().device_enforcement() {
            return Err(Error::VolumeRefused { capability });
        }
        Ok(())
    }
}

/// What a grant lets its recipient pull.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    /// Every snapshot of the Volume: a Mirror's, until it is withdrawn.
    Standing,
    /// Only these snapshots, which this Machine took under the operation.
    Snapshots(Vec<SnapshotName>),
}

/// This Machine's Volume operations, grants and withdrawals, kept in the approvals log.
/// No record here is pruned.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct VolumeRecords {
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

/// What the source of a Move did after its cut. Written once.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Resumed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Grant {
    operation: OperationId,
    volume: DockerVolumeName,
    recipient: MachineId,
    scope: GrantScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Withdrawal {
    volume: DockerVolumeName,
    recipient: MachineId,
    at: u64,
}

impl VolumeRecords {
    pub(super) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn admit(
        &mut self,
        operation: OperationId,
        digest: StepDigest,
        machine: &MachineId,
        volume: &DockerVolumeName,
        step: &VolumeStep,
        verify: impl FnOnce() -> Result<VolumeWorkflow, AuthorityRefusal>,
    ) -> Result<(), Error> {
        let refused = |field| {
            Err(Error::VolumeStepRefused(AuthorityRefusal::Mismatch {
                field,
            }))
        };
        if let Some(record) = self.operations.get(&operation) {
            if record.state == OperationState::Ended {
                return Err(Error::VolumeStepRefused(AuthorityRefusal::Replayed {
                    operation,
                }));
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
        let workflow = verify().map_err(Error::VolumeStepRefused)?;
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

    fn end(&mut self, operation: OperationId) {
        if let Some(record) = self.operations.get_mut(&operation) {
            record.state = OperationState::Ended;
        }
        self.grants.retain(|grant| grant.operation != operation);
    }

    fn grant(
        &mut self,
        operation: OperationId,
        recipient: MachineId,
        scope: GrantScope,
    ) -> Result<(), Error> {
        let refused = |refusal| Err(Error::VolumeStepRefused(refusal));
        let Some(record) = self.operations.get(&operation) else {
            return refused(AuthorityRefusal::Mismatch {
                field: "an operation",
            });
        };
        if record.state == OperationState::Ended {
            return refused(AuthorityRefusal::Replayed { operation });
        }
        let target =
            matches!(&record.workflow, VolumeWorkflow::Move { to, .. } if *to == recipient);
        let sends = record.workflow.mirrors_to(&recipient)
            || (target && matches!(scope, GrantScope::Snapshots(_)));
        if !sends {
            return refused(AuthorityRefusal::Mismatch {
                field: "a recipient",
            });
        }
        let outranked = self.withdrawals.iter().any(|withdrawal| {
            withdrawal.volume == record.volume
                && withdrawal.recipient == recipient
                && withdrawal.at > record.admitted
        });
        if outranked {
            return refused(AuthorityRefusal::Replayed { operation });
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

    fn live(
        &self,
        volume: &DockerVolumeName,
        recipient: &MachineId,
        snapshot: &SnapshotName,
    ) -> bool {
        self.grants.iter().any(|grant| {
            grant.volume == *volume
                && grant.recipient == *recipient
                && match &grant.scope {
                    GrantScope::Standing => true,
                    GrantScope::Snapshots(names) => names.contains(snapshot),
                }
        })
    }

    /// End every open operation here that mirrors `volume` to `recipient`: a Mirror's own,
    /// on either Machine, and a Move's following on the recipient itself. A Move between
    /// two other Machines goes on, but can no longer grant `recipient` anything.
    fn withdraw(&mut self, machine: &MachineId, volume: &DockerVolumeName, recipient: &MachineId) {
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
}

/// What a refused Volume verb answers, by the capability refused.
pub(super) fn refusal_text(capability: &str) -> &'static str {
    if capability == SEED_VOLUME_CAPABILITY {
        SEED_REFUSED_BY_ENFORCEMENT
    } else {
        VOLUME_START_REFUSED_BY_ENFORCEMENT
    }
}

#[cfg(test)]
#[path = "volume_approval_tests.rs"]
mod tests;
