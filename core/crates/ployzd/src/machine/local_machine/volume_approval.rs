//! The Volume verbs an enforcing Machine refuses from an Operator caller, and this
//! Machine's own record of the Volume operations a Device approved, kept as
//! [`VolumeRecords`] in the approvals log.
//!
//! An operation is admitted once inside its window and then runs on its record, so the
//! work a person started finishes after the window. An ended operation never runs again.
//! Each record is synced before the effect it guards, and none reads a clock again.

use std::net::IpAddr;

use ployz_core::{
    ContainerId, ContainerKind, CopyTransition, DockerVolumeName, MachineId, ManagementAddress,
    OperationId, Sealing, SnapshotName, Start,
    authority::{
        Approver, AuthorityRefusal, Purpose, Signed, StepDigest, Timestamp, VolumeApproval,
        VolumeFacts, VolumeStep, VolumeWorkflow,
        deploy::{self, Creation},
        volume,
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
    /// A cut needs a Move approval from this Machine. A promote with one is admitted: on a
    /// Move's source it records the resume, which ends the Move here, and on its target it
    /// grants each Mirror that keeps following every snapshot. A promote without one runs
    /// as before. Returns the operation admitted, for the step's effect to record against.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] for a pull or a cut with no approval, a pull
    /// from another source, and a step the approval does not admit; [`Error::ApprovalRecord`]
    /// or a store error when a record cannot be read.
    pub(crate) async fn admit_operator_copy(
        &self,
        approval: Option<&Signed<VolumeApproval>>,
        volume: &DockerVolumeName,
        step: CopyStep<'_>,
    ) -> Result<Option<Admitted>, Error> {
        if !self.record().device_enforcement() {
            return Ok(None);
        }
        let unsigned = || {
            Error::VolumeStepRefused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Volume,
            })
        };
        match step {
            CopyStep::Pull { transition, from } => {
                let approval = approval.ok_or_else(unsigned)?;
                self.admit_source(approval, from).await?;
                self.admitted(approval, volume, &VolumeStep::Copy(transition))
                    .map(Some)
            }
            CopyStep::Held { from } => {
                let Some(approval) = approval else {
                    return Ok(None);
                };
                self.admit_source(approval, from).await?;
                let step = VolumeStep::Copy(CopyTransition::MirrorToMirror);
                self.admitted(approval, volume, &step).map(Some)
            }
            CopyStep::Snapshot => {
                let Some(approval) = approval else {
                    return Ok(None);
                };
                let step = VolumeStep::Copy(CopyTransition::WriterToSnapshot);
                let admitted = self.admitted(approval, volume, &step)?;
                if let VolumeWorkflow::Mirror { mirror, .. } = admitted.workflow {
                    self.grant_volume(admitted.operation, mirror, GrantScope::Standing)?;
                }
                Ok(Some(admitted))
            }
            CopyStep::Cut => {
                let approval = approval.ok_or_else(unsigned)?;
                let step = VolumeStep::Copy(CopyTransition::WriterToMirror);
                self.admitted(approval, volume, &step).map(Some)
            }
            CopyStep::Promote { transition } => {
                let Some(approval) = approval else {
                    return Ok(None);
                };
                let admitted = self.admitted(approval, volume, &VolumeStep::Copy(transition))?;
                let machine = self.record().id();
                if let VolumeWorkflow::Move {
                    from, to, mirrors, ..
                } = &admitted.workflow
                {
                    if *from == machine {
                        self.resume_volume(admitted.operation)?;
                    }
                    if *to == machine && transition == CopyTransition::MirrorToWriter {
                        for mirror in mirrors {
                            self.grant_volume(admitted.operation, *mirror, GrantScope::Standing)?;
                        }
                    }
                }
                Ok(Some(admitted))
            }
        }
    }

    fn admitted(
        &self,
        approval: &Signed<VolumeApproval>,
        volume: &DockerVolumeName,
        step: &VolumeStep,
    ) -> Result<Admitted, Error> {
        let operation = self.admit_volume_step(approval, volume, step)?;
        let workflow = approval.open().map_err(Error::VolumeStepRefused)?.workflow;
        Ok(Admitted {
            operation,
            workflow,
        })
    }

    /// Record that this Machine, a Move's source, resumes: the Move ends here, with its
    /// grants, before the Writer route makes the root writable again.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be written.
    fn resume_volume(&self, operation: OperationId) -> Result<(), Error> {
        self.approvals
            .change_volumes(Timestamp::now(), |records, _| {
                records.resume(operation);
                Ok(())
            })?;
        tracing::info!(%operation, "Volume move resumed here; it ends on this Server");
        Ok(())
    }

    /// Record the cut of a Move this Machine admitted, before it stops anything: the snapshot
    /// it takes, and the Containers in `running` it is about to stop. The Move's target may
    /// pull that snapshot from then on.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] for a second cut at another snapshot or a Move
    /// ended here, and [`Error::ApprovalRecord`] when the record cannot be written.
    pub(crate) fn record_cut(
        &self,
        operation: OperationId,
        snapshot: &SnapshotName,
        running: &[ContainerId],
    ) -> Result<(), Error> {
        let machine = self.record().id();
        self.approvals
            .change_volumes(Timestamp::now(), |records, _| {
                records
                    .cut(operation, &machine, snapshot, running)
                    .map_err(Error::VolumeStepRefused)
            })?;
        tracing::info!(%operation, %snapshot, stopping = running.len(), "Volume cut recorded");
        Ok(())
    }

    /// Let a Move's target pull `snapshot`, which this Machine, the Move's source, left as
    /// its newest under the Move.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] when the Move is no longer open here, and
    /// [`Error::ApprovalRecord`] when the record cannot be written.
    pub(crate) fn grant_move_snapshot(
        &self,
        admitted: &Admitted,
        snapshot: &SnapshotName,
    ) -> Result<(), Error> {
        let VolumeWorkflow::Move { from, to, .. } = &admitted.workflow else {
            return Ok(());
        };
        if *from != self.record().id() {
            return Ok(());
        }
        self.grant_volume(
            admitted.operation,
            *to,
            GrantScope::Snapshots(vec![snapshot.clone()]),
        )
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

    /// Under Device enforcement, refuse an Operator's `SetVolumeCopy` that starts the
    /// Writer's Container with no Volume approval, before the switch lock.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] as unsigned under enforcement.
    pub(crate) fn refuse_unapproved_volume_start(
        &self,
        approval: Option<&Signed<VolumeApproval>>,
    ) -> Result<(), Error> {
        if approval.is_none() && self.record().device_enforcement() {
            return Err(Error::VolumeStepRefused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Volume,
            }));
        }
        Ok(())
    }

    /// Under Device enforcement, admit an Operator's Writer `start` on `volume` against the
    /// Move approval it carries, before the transition runs. A Create must be the Service
    /// the Move approves, by its [`deploy::PayloadDigest`] in the approved Namespace, the
    /// check a Deploy's creation gets. A Kept start must name a Container this operation's
    /// cut stopped on this Machine, whatever it mounts.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] for no approval, a workflow that is not a Move,
    /// a spec that differs from the approved one, a Container the cut did not stop, and a
    /// start the approval does not admit; [`Error::ApprovalRecord`] when a record cannot be
    /// read or written.
    pub(crate) fn admit_operator_volume_start(
        &self,
        approval: Option<&Signed<VolumeApproval>>,
        volume: &DockerVolumeName,
        start: &Start,
    ) -> Result<(), Error> {
        if !self.record().device_enforcement() {
            return Ok(());
        }
        let refused = |refusal: AuthorityRefusal| {
            tracing::warn!(%volume, "Volume start refused: {refusal}");
            Error::VolumeStepRefused(refusal)
        };
        let approval = approval.ok_or_else(|| {
            refused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Volume,
            })
        })?;
        let body = approval.open().map_err(refused)?;
        let VolumeWorkflow::Move { service, .. } = &body.workflow else {
            return Err(refused(AuthorityRefusal::Mismatch {
                field: "a Volume step",
            }));
        };
        if let Start::Create {
            namespace,
            resolved_spec,
            ..
        } = start
        {
            if *namespace != body.namespace {
                return Err(refused(AuthorityRefusal::Mismatch {
                    field: "a Namespace",
                }));
            }
            deploy::refuse_preview_parts(namespace, resolved_spec).map_err(refused)?;
            deploy::approved_creation(
                service,
                &body.namespace,
                Creation {
                    service: &resolved_spec.name,
                    kind: ContainerKind::ServiceContainer,
                    spec: resolved_spec,
                },
            )
            .map_err(refused)?;
        }
        let operation = self.admit_volume_step(approval, volume, &VolumeStep::Start)?;
        if let Start::Kept { container_id } = start {
            let stopped = self
                .approvals
                .read_volumes(|records| records.stopped_by_cut(operation, container_id))?;
            if !stopped {
                return Err(refused(AuthorityRefusal::Mismatch {
                    field: "a Container",
                }));
            }
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
    /// The cut on a Move's source.
    Cut,
    /// The copy becomes the Writer: a Move's target promotes, or its source resumes.
    Promote { transition: CopyTransition },
}

/// A Volume step this Machine admitted, for its effect to record against.
#[derive(Clone, Debug)]
pub(crate) struct Admitted {
    pub(crate) operation: OperationId,
    pub(crate) workflow: VolumeWorkflow,
}

#[cfg(test)]
#[path = "volume_approval_tests.rs"]
mod tests;
