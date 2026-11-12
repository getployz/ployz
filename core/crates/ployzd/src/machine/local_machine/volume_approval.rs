//! The Volume verbs an enforcing Machine refuses from an Operator caller, and this
//! Machine's own record of the Volume operations a Device approved, kept as
//! [`VolumeRecords`] in the approvals log.
//!
//! An operation is admitted once inside its window and then runs on its record, so the
//! work a person started finishes after the window. An ended operation never runs again.
//! Each record is synced before the effect it guards, and none reads a clock again.

use std::net::IpAddr;

use ployz_core::{
    ContainerId, ContainerKind, CopyTransition, DockerVolumeName, HandoffReceiptEnvelope,
    MachineId, ManagementAddress, OperationId, Sealing, SlotRole, SnapshotGuid, SnapshotName,
    Start, VolumeCopy, WriterEpoch,
    authority::{
        Approver, AuthorityRefusal, Handed as Lands, HandoffFacts, HandoffReceipt, MachineSigned,
        Purpose, Signed, StepDigest, Timestamp, VolumeApproval, VolumeFacts, VolumeStep,
        VolumeWorkflow,
        deploy::{self, Creation},
        volume,
    },
};
use ployz_decide::volume_records::Handed;
pub(crate) use ployz_decide::volume_records::{GrantScope, VolumeRecords};

use super::{Error, LocalMachine};
use crate::{sealing_key::SealingKey, volume_send::Sends};

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
        self.admit_handed_step(approval, volume, step, None)
    }

    /// [`Self::admit_volume_step`], where `handed`, a receipt this Machine checked, proves
    /// the Move started on its source inside the window and so stands in for it.
    fn admit_handed_step(
        &self,
        approval: &Signed<VolumeApproval>,
        volume: &DockerVolumeName,
        step: &VolumeStep,
        handed: Option<&HandoffReceipt>,
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
                                handed,
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
    /// A cut needs a Move approval from this Machine. A promote needs an approval: a Move's,
    /// where on its source it records the resume, which ends the Move here, and on its
    /// target it grants each Mirror that keeps following every snapshot; or a Restore's of
    /// this Machine's copy at the snapshot it names. A Forget of a slot holding a snapshot
    /// needs a removal of that slot in its role, a replacing Mirror before its first pull, or
    /// the Move this Machine was the target of; a slot holding none destroys nothing, so it
    /// needs none. Returns the operation admitted, for the step's effect to record against.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] for a pull, a cut, a promote or a Forget of a
    /// snapshot with no approval, a pull from another source, a Restore at another snapshot,
    /// a removal of another role, and a step the approval does not admit;
    /// [`Error::ApprovalRecord`] or a store error when a record cannot be read.
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
            CopyStep::Pull {
                transition,
                from,
                target,
                receipt,
            } => {
                let approval = approval.ok_or_else(unsigned)?;
                self.admit_source(approval, from).await?;
                let handed = receipt
                    .map(|receipt| {
                        self.checked_receipt(
                            approval,
                            volume,
                            receipt,
                            Lands::Pull { snapshot: target },
                        )
                    })
                    .transpose()?;
                self.admitted(
                    approval,
                    volume,
                    &VolumeStep::Copy(transition),
                    handed.as_ref(),
                )
                .map(Some)
            }
            CopyStep::Held { from } => {
                let Some(approval) = approval else {
                    return Ok(None);
                };
                self.admit_source(approval, from).await?;
                let step = VolumeStep::Copy(CopyTransition::MirrorToMirror);
                self.admitted(approval, volume, &step, None).map(Some)
            }
            CopyStep::Snapshot => {
                let Some(approval) = approval else {
                    return Ok(None);
                };
                let step = VolumeStep::Copy(CopyTransition::WriterToSnapshot);
                let admitted = self.admitted(approval, volume, &step, None)?;
                if let VolumeWorkflow::Mirror { mirror, .. } = admitted.workflow {
                    self.grant_volume(admitted.operation, mirror, GrantScope::Standing)?;
                }
                Ok(Some(admitted))
            }
            CopyStep::Cut => {
                let approval = approval.ok_or_else(unsigned)?;
                let step = VolumeStep::Copy(CopyTransition::WriterToMirror);
                self.admitted(approval, volume, &step, None).map(Some)
            }
            CopyStep::Forget { role, empty } => {
                if empty {
                    return Ok(None);
                }
                let approval = approval.ok_or_else(unsigned)?;
                let body = approval.open().map_err(Error::VolumeStepRefused)?;
                if let VolumeWorkflow::MirrorRemoval { role: removed, .. } = body.workflow
                    && role != Some(removed)
                {
                    return Err(Error::VolumeStepRefused(AuthorityRefusal::Mismatch {
                        field: "a copy's role",
                    }));
                }
                let step = VolumeStep::Copy(CopyTransition::MirrorToForget);
                self.admitted(approval, volume, &step, None).map(Some)
            }
            CopyStep::Promote {
                transition,
                at,
                epoch,
                receipt,
            } => {
                let approval = approval.ok_or_else(unsigned)?;
                let machine = self.record().id();
                let step = VolumeStep::Copy(transition);
                let body = approval.open().map_err(Error::VolumeStepRefused)?;
                if let VolumeWorkflow::Restore { at: restored, .. } = body.workflow
                    && restored != *at
                {
                    return Err(Error::VolumeStepRefused(AuthorityRefusal::Mismatch {
                        field: "a snapshot",
                    }));
                }
                if !matches!(&body.workflow, VolumeWorkflow::Move { to, .. } if *to == machine) {
                    let admitted = self.admitted(approval, volume, &step, None)?;
                    if admitted.workflow.moves_from(&machine) {
                        self.resume_volume(admitted.operation)?;
                    }
                    return Ok(Some(admitted));
                }
                let receipt =
                    receipt.ok_or(Error::VolumeStepRefused(AuthorityRefusal::Unreceipted))?;
                let handed =
                    self.checked_receipt(approval, volume, receipt, Lands::Writer { at, epoch })?;
                let admitted = self.admitted(approval, volume, &step, Some(&handed))?;
                self.approvals
                    .change_volumes(Timestamp::now(), |records, _| {
                        records
                            .promote(admitted.operation, &machine, (*at, epoch), receipt.clone())
                            .map_err(Error::VolumeStepRefused)
                    })?;
                tracing::info!(
                    operation = %admitted.operation,
                    from = %handed.from,
                    %at,
                    "Volume move promoted here on its source's receipt; it ends on this Server"
                );
                Ok(Some(admitted))
            }
        }
    }

    /// On a Move's source after its cut, record that it hands `volume` to `to`, which becomes
    /// the Writer at the cut's snapshot, held here as `at` and `snapshot`, with `epoch`. The
    /// decision is synced before this returns the receipt, and a resend returns the same one.
    /// After it this Machine never becomes the Writer again under the Move.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] for an approval that is not a Move from here to
    /// `to`, a Move with no cut at that snapshot, a Move this Machine resumed, a
    /// second handoff that differs, and a Machine holding no Sealing Key;
    /// [`Error::ApprovalRecord`] when the record cannot be read or written.
    pub(crate) fn hand_off(
        &self,
        approval: &Signed<VolumeApproval>,
        volume: &DockerVolumeName,
        to: MachineId,
        (at, snapshot): (SnapshotGuid, &SnapshotName),
        epoch: WriterEpoch,
    ) -> Result<HandoffReceiptEnvelope, Error> {
        let operation = self.admit_volume_step(approval, volume, &VolumeStep::Handoff)?;
        let machine = self.record().id();
        let handed = self
            .approvals
            .change_volumes(Timestamp::now(), |records, _| {
                records
                    .handoff(operation, &machine, to, (at, snapshot), epoch)
                    .map_err(Error::VolumeStepRefused)
            })?;
        tracing::info!(
            %operation,
            %volume,
            %to,
            %snapshot,
            "Volume move handed off here; this Server never writes it again under the move"
        );
        self.receipt_of(&handed)
    }

    /// The receipt this Machine signed when it handed `copy` of `volume` off, if it did: an
    /// Operator's Writer want on that copy is refused with it, approved or not.
    ///
    /// # Errors
    /// Returns [`Error::ApprovalRecord`] when the record cannot be read, and
    /// [`Error::VolumeStepRefused`] when this Machine holds no Sealing Key to sign with.
    pub(crate) fn handed_off(
        &self,
        volume: &DockerVolumeName,
        copy: Option<&VolumeCopy>,
    ) -> Result<Option<HandoffReceiptEnvelope>, Error> {
        let Some(VolumeCopy::Slot {
            newest: Some(newest),
            ..
        }) = copy
        else {
            return Ok(None);
        };
        let handed = self
            .approvals
            .read_volumes(|records| records.handed_at(volume, &newest.guid))?;
        handed.map(|handed| self.receipt_of(&handed)).transpose()
    }

    /// The receipt for `handed`, signed with the Cluster's receipt key. Ed25519 signs one
    /// body one way, so a resend carries the same bytes.
    fn receipt_of(&self, handed: &Handed) -> Result<HandoffReceiptEnvelope, Error> {
        let key = self.receipt_key()?;
        let receipt = HandoffReceipt {
            cluster: key.public_key(),
            operation: handed.operation,
            approval: handed.digest,
            volume: handed.volume.clone(),
            from: self.record().id(),
            to: handed.handoff.to,
            at: handed.handoff.at,
            snapshot: handed.handoff.snapshot.clone(),
            from_epoch: handed.handoff.from_epoch,
            epoch: handed.handoff.epoch,
        };
        MachineSigned::sign(&receipt, &key.receipt_signer()).map_err(|error| {
            Error::VolumeStepRefused(AuthorityRefusal::MalformedMachineStatement {
                kind: "volume-handoff",
                problem: error.to_string(),
            })
        })
    }

    /// Check `receipt` is this Cluster's statement that `approval`'s Move handed `volume` to
    /// this Machine at the snapshot the step lands on.
    fn checked_receipt(
        &self,
        approval: &Signed<VolumeApproval>,
        volume: &DockerVolumeName,
        receipt: &HandoffReceiptEnvelope,
        at: Lands<'_>,
    ) -> Result<HandoffReceipt, Error> {
        let key = self.receipt_key()?;
        let body = approval.open().map_err(Error::VolumeStepRefused)?;
        volume::verify_receipt(
            receipt,
            &HandoffFacts {
                key: key.receipt_signer().public(),
                cluster: key.public_key(),
                machine: &self.record().id(),
                operation: body.operation,
                approval: StepDigest::of(&approval.body),
                volume,
                at,
            },
        )
        .map_err(|refusal| {
            tracing::warn!(%volume, "Volume handoff receipt refused: {refusal}");
            Error::VolumeStepRefused(refusal)
        })
    }

    /// The Sealing Key this Machine holds, which its receipt key derives from.
    fn receipt_key(&self) -> Result<SealingKey, Error> {
        let held = self
            .held_sealing(self.record().phase())
            .and_then(|_| SealingKey::load(self.owner.data_dir()).ok().flatten());
        held.ok_or(Error::VolumeStepRefused(
            AuthorityRefusal::NoClusterIdentity,
        ))
    }

    fn admitted(
        &self,
        approval: &Signed<VolumeApproval>,
        volume: &DockerVolumeName,
        step: &VolumeStep,
        handed: Option<&HandoffReceipt>,
    ) -> Result<Admitted, Error> {
        let operation = self.admit_handed_step(approval, volume, step, handed)?;
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
                records.resume(operation).map_err(Error::VolumeStepRefused)
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
        (snapshot, epoch): (&SnapshotName, WriterEpoch),
        running: &[ContainerId],
    ) -> Result<(), Error> {
        let machine = self.record().id();
        self.approvals
            .change_volumes(Timestamp::now(), |records, _| {
                records
                    .cut(operation, &machine, snapshot, epoch, running)
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
    /// cut stopped on this Machine, whatever it mounts. With `handed`, the source's receipt
    /// for the Writer at its snapshot and epoch, the start is admitted after the window too,
    /// as the promotion it comes with is.
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
        handed: Option<(&HandoffReceiptEnvelope, &SnapshotGuid, WriterEpoch)>,
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
        let handed = handed
            .map(|(receipt, at, epoch)| {
                self.checked_receipt(approval, volume, receipt, Lands::Writer { at, epoch })
            })
            .transpose()?;
        let operation =
            self.admit_handed_step(approval, volume, &VolumeStep::Start, handed.as_ref())?;
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
    /// A pull that makes or refreshes a Mirror of `target`, from `from`, with the Move
    /// source's receipt for it when the request carries one.
    Pull {
        transition: CopyTransition,
        from: &'a ManagementAddress,
        target: &'a SnapshotName,
        receipt: Option<&'a HandoffReceiptEnvelope>,
    },
    /// A pull of the snapshot the slot already holds. Checked as a refresh when it carries
    /// an approval, so a Mirror approved again while caught up admits it; passed without one,
    /// as before.
    Held { from: &'a ManagementAddress },
    /// A run snapshot on the Writer, taken or already held.
    Snapshot,
    /// The cut on a Move's source.
    Cut,
    /// The copy becomes the Writer at `at` with `epoch`: a Move's target promotes, which
    /// needs its source's `receipt`, its source resumes, or a Restore promotes it.
    Promote {
        transition: CopyTransition,
        at: &'a SnapshotGuid,
        epoch: WriterEpoch,
        receipt: Option<&'a HandoffReceiptEnvelope>,
    },
    /// A Mirror's slot is destroyed: one held as `role`, which is `empty` when it holds no
    /// snapshot.
    Forget { role: Option<SlotRole>, empty: bool },
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
