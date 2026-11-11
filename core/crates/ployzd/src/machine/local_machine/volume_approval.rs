//! The Volume verbs an enforcing Machine refuses from an Operator caller, and this
//! Machine's own record of the Volume operations a Device approved, kept as
//! [`VolumeRecords`] in the approvals log.
//!
//! An operation is admitted once inside its window and then runs on its record, so the
//! work a person started finishes after the window. An ended operation never runs again.
//! Each record is synced before the effect it guards, and none reads a clock again.

use std::net::IpAddr;

use ployz_core::{
    CopyTransition, DockerVolumeName, MachineId, ManagementAddress, OperationId,
    SEED_REFUSED_BY_ENFORCEMENT, SEED_VOLUME_CAPABILITY, SET_VOLUME_COPY_CAPABILITY, Sealing,
    SnapshotName, VOLUME_START_REFUSED_BY_ENFORCEMENT,
    authority::{
        Approver, AuthorityRefusal, Purpose, Signed, StepDigest, Timestamp, VolumeApproval,
        VolumeFacts, VolumeStep, VolumeWorkflow, volume,
    },
};
pub(crate) use ployz_decide::volume_records::{GrantScope, VolumeRecords};

use super::{Error, LocalMachine};
use crate::volume_send::Sends;

impl LocalMachine {
    /// Admit `step` on `volume` under `approval`. An operation this Machine ended is
    /// refused for good. An operation open here passes on its record, at any time, for any
    /// step its workflow permits this Machine. Otherwise the approval must verify inside its
    /// window, and the operation is recorded open, synced, before this returns. Admitting
    /// an operation that owns this Machine's copy ends the one that owned it before.
    /// Admitting a replacing Mirror's opening `MirrorToForget` also withdraws this Machine
    /// from the Volume in that same write, just before the replacement opens, so the
    /// withdrawal ends every other Mirror operation here and none of the replacement's. The
    /// caller writes no other withdrawal for that `Forget`.
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
        let mut by = None;
        self.approvals
            .change_volumes(Timestamp::now(), |records, now| {
                records
                    .admit(operation, digest, &machine, volume, step, || {
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
                        .map(|approved| {
                            by = Some(Approver::Device(approved.by).to_string());
                            approved.body.workflow
                        })
                    })
                    .map_err(Error::VolumeStepRefused)
            })?;
        match by {
            Some(by) => tracing::info!(?step, "Volume {operation} approved by {by} for {volume}"),
            None => tracing::info!(%operation, %volume, ?step, "Volume step admitted"),
        }
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
                records
                    .grant(operation, recipient, scope)
                    .map_err(Error::VolumeStepRefused)
            })
    }

    /// Whether a live grant lets `recipient` pull `snapshot` of `volume` from here, or,
    /// with no snapshot, any of it.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read.
    pub fn volume_grant_live(
        &self,
        volume: &DockerVolumeName,
        recipient: &MachineId,
        snapshot: Option<&SnapshotName>,
    ) -> Result<bool, Error> {
        self.approvals
            .read_volumes(|records| records.live(volume, recipient, snapshot))
    }

    /// Withdraw `recipient` from `volume` here, before a `Forget` destroys anything: every
    /// Mirror operation this Machine admitted for it ends, and so does every grant to it. It
    /// needs no approval, because it only narrows what a driver can do. An approval this
    /// Machine never admitted is still new work while its window lasts. A Machine that
    /// doesn't enforce writes one only when it holds something the withdrawal ends. Returns
    /// whether it wrote one.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub fn withdraw_volume(
        &self,
        volume: &DockerVolumeName,
        recipient: &MachineId,
    ) -> Result<bool, Error> {
        let record = self.record();
        let machine = record.id();
        let enforcing = record.device_enforcement();
        let withdrew = self
            .approvals
            .change_volumes(Timestamp::now(), |records, _| {
                let withdraws = enforcing || records.sends(volume, recipient);
                if withdraws {
                    records.withdraw(&machine, volume, recipient);
                }
                Ok(withdraws)
            })?;
        if withdrew {
            tracing::info!(%volume, %recipient, "Volume recipient withdrawn");
        }
        Ok(withdrew)
    }

    /// Under Device enforcement, admit an Operator's `SetVolumeCopy` step against the Volume
    /// approval it carries. A pull needs one that lets this Machine pull, and its `from`
    /// must be the address this Machine's own accepted row gives a Server the approval pulls
    /// from. A Snapshot with one opens its operation, and under a Mirror approval grants the
    /// Mirror every snapshot of the Volume. A Snapshot without one runs and records nothing.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] for a pull with no approval, from another
    /// source, or that the approval does not admit, and a Snapshot whose approval does not
    /// admit it; [`Error::ApprovalRecord`] or a store error when a record cannot be read.
    pub(crate) async fn admit_operator_copy(
        &self,
        approval: Option<&Signed<VolumeApproval>>,
        volume: &DockerVolumeName,
        step: CopyStep<'_>,
    ) -> Result<(), Error> {
        if !self.record().device_enforcement() {
            return Ok(());
        }
        match step {
            CopyStep::Pull { transition, from } => {
                let Some(approval) = approval else {
                    return Err(Error::VolumeStepRefused(AuthorityRefusal::Unsigned {
                        purpose: Purpose::Volume,
                    }));
                };
                self.admit_source(approval, from).await?;
                self.admit_volume_step(approval, volume, &VolumeStep::Copy(transition))?;
            }
            CopyStep::Held { from } => {
                let Some(approval) = approval else {
                    return Ok(());
                };
                self.admit_source(approval, from).await?;
                let step = VolumeStep::Copy(CopyTransition::MirrorToMirror);
                self.admit_volume_step(approval, volume, &step)?;
            }
            CopyStep::Snapshot => {
                let Some(approval) = approval else {
                    return Ok(());
                };
                let step = VolumeStep::Copy(CopyTransition::WriterToSnapshot);
                let operation = self.admit_volume_step(approval, volume, &step)?;
                let body = approval.open().map_err(Error::VolumeStepRefused)?;
                if let VolumeWorkflow::Mirror { mirror, .. } = body.workflow {
                    self.grant_volume(operation, mirror, GrantScope::Standing)?;
                }
            }
        }
        Ok(())
    }

    /// Refuse a pull whose `from` is not the management address this Machine's own accepted
    /// row gives a Server `approval` lets it pull from. An approval that lets this Machine
    /// pull from nobody is left for admission to refuse by name.
    async fn admit_source(
        &self,
        approval: &Signed<VolumeApproval>,
        from: &ManagementAddress,
    ) -> Result<(), Error> {
        let body = approval.open().map_err(Error::VolumeStepRefused)?;
        let sources = body.workflow.pulls_from(&self.record().id());
        if sources.is_empty() {
            return Ok(());
        }
        let replicated = self.replicated()?;
        for source in sources {
            if let Some(row) = replicated.machine(source.as_str()).await?
                && row.management_address() == *from
            {
                return Ok(());
            }
        }
        Err(Error::VolumeStepRefused(AuthorityRefusal::Mismatch {
            field: "the source Server",
        }))
    }

    /// Whether this Machine sends `snapshot` of `volume`, or with none its bound, to the
    /// mesh address `peer`. A Machine that doesn't enforce sends to any. Under enforcement
    /// the address must be the management address of a Machine this one accepts, holding a
    /// live grant that covers the snapshot.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] or a store error when a record cannot be read.
    pub(crate) async fn sends_volume(
        &self,
        peer: IpAddr,
        volume: &DockerVolumeName,
        snapshot: Option<&SnapshotName>,
    ) -> Result<Sends, Error> {
        if !self.record().device_enforcement() {
            return Ok(Sends::Open);
        }
        let IpAddr::V6(address) = peer.to_canonical() else {
            return Ok(Sends::Refused { recipient: None });
        };
        let machines = self.replicated()?.machines().await?;
        let Some(recipient) = machines
            .observations
            .into_iter()
            .find(|machine| machine.management_address().0 == address)
        else {
            return Ok(Sends::Refused { recipient: None });
        };
        if self.volume_grant_live(volume, &recipient.id, snapshot)? {
            Ok(Sends::Granted(recipient.id))
        } else {
            Ok(Sends::Refused {
                recipient: Some(recipient.name),
            })
        }
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

/// The step of an Operator's `SetVolumeCopy` an enforcing Machine checks against its
/// Volume approval.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CopyStep<'a> {
    /// A pull that makes or refreshes a Mirror, from `from`.
    Pull {
        transition: CopyTransition,
        from: &'a ManagementAddress,
    },
    /// A pull of the snapshot the slot already holds. Checked as a refresh when it carries
    /// an approval, so a Mirror approved again while caught up admits it; passed without one,
    /// as before.
    Held { from: &'a ManagementAddress },
    /// A run snapshot on the Writer, taken or already held.
    Snapshot,
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
