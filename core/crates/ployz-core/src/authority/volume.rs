//! The approval a person's Device signs for one Volume workflow, the steps each workflow
//! lets each Machine take, and the checks a Machine runs before it admits the first step.
//!
//! A workflow is a graph, not a list of routes: [`VolumeWorkflow::permits`] answers for
//! one Machine and one step. A Machine keeps its own record of the operations it admitted,
//! so a later step of an open operation needs no fresh window and an ended one never runs
//! again. That record is the Machine's, not this module's.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::{self, Timestamp},
    deploy::{ApprovedChange, ApprovedService},
    keys::{
        Approver, MachineSigned, MachineStatement, Purpose, Signed, SigningPublicKey, Statement,
    },
    proposal::StepDigest,
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::SignerKey,
    shown::Shown,
    trusted_key::TrustedKeys,
};
use crate::{
    CopyTransition, DockerVolumeName, MachineId, Namespace, OperationId,
    ProvisionedVolumeMaximumBytes, SealingPublicKey, SlotRole, SnapshotGuid, SnapshotName,
    WriterEpoch,
};

/// A person approves one workflow on one Volume in one Cluster, under one operation whose
/// window bounds when its first step may start on each Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumeApproval {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    pub operation: OperationId,
    pub namespace: Namespace,
    /// The Docker Volume the workflow acts on. A seed names the Volume it creates.
    pub volume: DockerVolumeName,
    pub workflow: VolumeWorkflow,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// The Device that signed, which the signature must come from.
    pub signer: SignerKey,
}

impl Statement for VolumeApproval {
    const PURPOSE: Purpose = Purpose::Volume;
}

/// What a person approves, with every Machine it reaches by ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeWorkflow {
    /// The Writer moves from `from` to `to`, which starts `service`. The Mirrors in
    /// `mirrors` keep following the new Writer.
    Move {
        from: MachineId,
        to: MachineId,
        service: ApprovedService,
        /// Sorted and distinct.
        #[serde(deserialize_with = "canonical::sorted_distinct")]
        mirrors: Vec<MachineId>,
    },
    /// `mirror` keeps a copy of the Writer on `writer`, and every later sync of it, until
    /// the Mirror is removed. With `replace` it first destroys the copy it holds.
    Mirror {
        writer: MachineId,
        mirror: MachineId,
        replace: bool,
    },
    /// `mirror` destroys its copy, held in `role`. `writer` is the Server that writes the
    /// Volume, or `None` when none does, so the copy may be the Volume's last.
    MirrorRemoval {
        mirror: MachineId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        writer: Option<MachineId>,
        role: SlotRole,
    },
    /// `machine` promotes its copy at `at` with no start, over a Move's handoff too. Writes
    /// after `loses_after` are lost.
    Restore {
        machine: MachineId,
        at: SnapshotGuid,
        loses_after: Timestamp,
    },
    /// Each of `machines` bounds the Volume at `maximum_bytes`. With `shrink` each request
    /// may lower a bound, and without it none may.
    Resize {
        /// Sorted and distinct.
        #[serde(deserialize_with = "canonical::sorted_distinct")]
        machines: Vec<MachineId>,
        maximum_bytes: ProvisionedVolumeMaximumBytes,
        shrink: bool,
    },
    /// `machine` creates the approval's Volume from `source` at snapshot `at`.
    Seed {
        machine: MachineId,
        source: DockerVolumeName,
        at: SnapshotGuid,
    },
}

/// One step a Machine is asked to take on a Volume, as its request reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VolumeStep {
    /// The `SetVolumeCopy` transition the planner chose for the copy this Machine holds.
    Copy(CopyTransition),
    /// Starting a Container on the Volume, after a Writer want's transition.
    Start,
    /// A Move's source giving the Volume up to its target, after its cut.
    Handoff,
    Seed {
        source: DockerVolumeName,
        at: SnapshotGuid,
    },
    Resize {
        maximum_bytes: ProvisionedVolumeMaximumBytes,
        allow_shrink: bool,
    },
}

impl VolumeWorkflow {
    /// Whether this workflow lets `machine` take `step`. A step outside the graph is
    /// refused however valid the signature.
    #[must_use]
    pub fn permits(&self, machine: &MachineId, step: &VolumeStep) -> bool {
        use CopyTransition::{
            MirrorToForget, MirrorToMirror, MirrorToWriter, NoneToMirror, WriterToMirror,
            WriterToSnapshot, WriterToWriter,
        };
        match (self, step) {
            (
                Self::Move {
                    from, to, mirrors, ..
                },
                VolumeStep::Copy(transition),
            ) => {
                (machine == from
                    && matches!(
                        transition,
                        WriterToSnapshot | WriterToMirror | WriterToWriter | MirrorToWriter
                    ))
                    || (machine == to
                        && matches!(
                            transition,
                            NoneToMirror
                                | MirrorToMirror
                                | MirrorToWriter
                                | WriterToWriter
                                | MirrorToForget
                        ))
                    || (mirrors.contains(machine) && *transition == MirrorToMirror)
            }
            (Self::Move { from, to, .. }, VolumeStep::Start) => machine == from || machine == to,
            (Self::Move { from, .. }, VolumeStep::Handoff) => machine == from,
            (
                Self::Mirror {
                    writer,
                    mirror,
                    replace,
                },
                VolumeStep::Copy(transition),
            ) => {
                (machine == writer && *transition == WriterToSnapshot)
                    || (machine == mirror
                        && (matches!(transition, NoneToMirror | MirrorToMirror)
                            || (*replace && *transition == MirrorToForget)))
            }
            (Self::MirrorRemoval { mirror, .. }, VolumeStep::Copy(transition)) => {
                machine == mirror && *transition == MirrorToForget
            }
            (Self::Restore { machine: owner, .. }, VolumeStep::Copy(transition)) => {
                machine == owner && matches!(transition, MirrorToWriter | WriterToWriter)
            }
            (
                Self::Resize {
                    machines,
                    maximum_bytes,
                    shrink,
                },
                VolumeStep::Resize {
                    maximum_bytes: wanted,
                    allow_shrink,
                },
            ) => machines.contains(machine) && wanted == maximum_bytes && shrink == allow_shrink,
            (
                Self::Seed {
                    machine: owner,
                    source,
                    at,
                },
                VolumeStep::Seed {
                    source: wanted,
                    at: wanted_at,
                },
            ) => machine == owner && wanted == source && wanted_at == at,
            _ => false,
        }
    }

    /// Whether this workflow drives `machine`'s copy: changes its role, destroys it, or,
    /// for a Mirror a Move keeps following, says where it pulls from. A Machine holds at
    /// most one open owner of a Volume's copy.
    #[must_use]
    pub fn owns(&self, machine: &MachineId) -> bool {
        match self {
            Self::Move {
                from, to, mirrors, ..
            } => machine == from || machine == to || mirrors.contains(machine),
            Self::Mirror { mirror, .. } | Self::MirrorRemoval { mirror, .. } => machine == mirror,
            Self::Restore { machine: owner, .. } => machine == owner,
            Self::Resize { .. } | Self::Seed { .. } => false,
        }
    }

    /// Whether this workflow sends the Volume to `recipient` as a Mirror: a Mirror's own,
    /// or one a Move keeps following.
    #[must_use]
    pub fn mirrors_to(&self, recipient: &MachineId) -> bool {
        match self {
            Self::Mirror { mirror, .. } => mirror == recipient,
            Self::Move { mirrors, .. } => mirrors.contains(recipient),
            Self::MirrorRemoval { .. }
            | Self::Restore { .. }
            | Self::Resize { .. }
            | Self::Seed { .. } => false,
        }
    }

    /// The Servers this workflow lets `machine` pull the Volume from: a Mirror's Writer, a
    /// Move's source for its target, and either side of a Move for a Mirror it keeps
    /// following. Empty when it lets `machine` pull from nobody.
    #[must_use]
    pub fn pulls_from(&self, machine: &MachineId) -> Vec<MachineId> {
        match self {
            Self::Mirror { writer, mirror, .. } if mirror == machine => vec![*writer],
            Self::Move { from, to, .. } if to == machine => vec![*from],
            Self::Move {
                from, to, mirrors, ..
            } if mirrors.contains(machine) => vec![*from, *to],
            Self::Move { .. }
            | Self::Mirror { .. }
            | Self::MirrorRemoval { .. }
            | Self::Restore { .. }
            | Self::Resize { .. }
            | Self::Seed { .. } => Vec::new(),
        }
    }

    /// Whether `machine` is the source of a Move under this workflow.
    #[must_use]
    pub fn moves_from(&self, machine: &MachineId) -> bool {
        matches!(self, Self::Move { from, .. } if from == machine)
    }

    fn names(&self, machine: &MachineId) -> bool {
        match self {
            Self::Move {
                from, to, mirrors, ..
            } => machine == from || machine == to || mirrors.contains(machine),
            Self::Mirror { writer, mirror, .. } => machine == writer || machine == mirror,
            Self::MirrorRemoval { mirror, writer, .. } => {
                machine == mirror || writer.as_ref() == Some(machine)
            }
            Self::Restore { machine: owner, .. } | Self::Seed { machine: owner, .. } => {
                machine == owner
            }
            Self::Resize { machines, .. } => machines.contains(machine),
        }
    }

    /// A Move to itself, or a Mirror of its own Writer, would let one Machine take both
    /// sides of the graph.
    fn distinct(&self) -> bool {
        match self {
            Self::Move {
                from, to, mirrors, ..
            } => from != to && !mirrors.contains(from) && !mirrors.contains(to),
            Self::Mirror { writer, mirror, .. } => writer != mirror,
            Self::MirrorRemoval { mirror, writer, .. } => writer.as_ref() != Some(mirror),
            Self::Restore { .. } | Self::Resize { .. } | Self::Seed { .. } => true,
        }
    }
}

/// What a person approved in a Volume approval, as a client shows it, computed from the
/// statement it signs. The Volume and each Server go by the names the client knows them by.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovedVolumeChange {
    /// Each Server in `servers` holds `volume` at `size`, and with `shrink` lowers a bigger
    /// copy to it. `size` reads as `ployz volume set --size` takes it, such as `20GB`.
    Resize {
        volume: String,
        servers: Vec<String>,
        size: String,
        shrink: bool,
    },
    /// `mirror` keeps a copy of `volume` from `writer`, and takes every later sync of it
    /// without another approval until the Mirror is removed. With `replace` it first
    /// destroys the copy it holds.
    Mirror {
        volume: String,
        writer: String,
        mirror: String,
        replace: bool,
    },
    /// `volume` moves from `from` to `to`. Its Service stops on `from` for the last copy and
    /// starts on `to` as `service` shows, and the Mirrors on `mirrors` keep following it.
    Move {
        volume: String,
        from: String,
        to: String,
        service: ApprovedChange,
        mirrors: Vec<String>,
    },
    /// `server` makes its copy of `volume` the Writer at snapshot `at`, starting nothing.
    /// Writes after `lost_after`, the time the client read the approval's `loses_after` as,
    /// are lost.
    Restore {
        volume: String,
        server: String,
        at: SnapshotGuid,
        lost_after: String,
    },
    /// `server` deletes its copy of `volume`, held as `role`. `writer` keeps writing the
    /// Volume, or with none the copy may be its last.
    MirrorRemoval {
        volume: String,
        server: String,
        role: SlotRole,
        writer: Option<String>,
    },
}

impl ApprovedVolumeChange {
    /// The change `approval` makes to the Volume the client calls `volume`, naming each Server
    /// with `name`. `None` for a workflow no client signs yet.
    #[must_use]
    pub fn of(
        approval: &VolumeApproval,
        volume: &str,
        name: impl Fn(&MachineId) -> String,
    ) -> Option<Self> {
        match &approval.workflow {
            VolumeWorkflow::Mirror {
                writer,
                mirror,
                replace,
            } => Some(Self::Mirror {
                volume: volume.to_owned(),
                writer: name(writer),
                mirror: name(mirror),
                replace: *replace,
            }),
            VolumeWorkflow::Resize {
                machines,
                maximum_bytes,
                shrink,
            } => Some(Self::Resize {
                volume: volume.to_owned(),
                servers: machines.iter().map(name).collect(),
                size: maximum_bytes.to_string().replace(' ', ""),
                shrink: *shrink,
            }),
            VolumeWorkflow::MirrorRemoval {
                mirror,
                writer,
                role,
            } => Some(Self::MirrorRemoval {
                volume: volume.to_owned(),
                server: name(mirror),
                role: *role,
                writer: writer.as_ref().map(&name),
            }),
            VolumeWorkflow::Move { .. }
            | VolumeWorkflow::Restore { .. }
            | VolumeWorkflow::Seed { .. } => None,
        }
    }

    /// The Restore `approval` makes of the Volume the client calls `volume`, reading its
    /// `loses_after` with `time`. `None` unless `approval` is a Restore.
    #[must_use]
    pub fn restoring(
        approval: &VolumeApproval,
        volume: &str,
        name: impl Fn(&MachineId) -> String,
        time: impl Fn(Timestamp) -> String,
    ) -> Option<Self> {
        let VolumeWorkflow::Restore {
            machine,
            at,
            loses_after,
        } = &approval.workflow
        else {
            return None;
        };
        Some(Self::Restore {
            volume: volume.to_owned(),
            server: name(machine),
            at: *at,
            lost_after: time(*loses_after),
        })
    }

    /// The Move `approval` makes of the Volume the client calls `volume`, with `service` the
    /// change the client computed for the Service it starts, from the spec whose digest the
    /// approval names. `None` unless `approval` is a Move of that Service.
    #[must_use]
    pub fn moving(
        approval: &VolumeApproval,
        volume: &str,
        name: impl Fn(&MachineId) -> String,
        service: ApprovedChange,
    ) -> Option<Self> {
        let VolumeWorkflow::Move {
            from,
            to,
            service: approved,
            mirrors,
        } = &approval.workflow
        else {
            return None;
        };
        if approved.name != service.service || approved.digest != service.digest {
            return None;
        }
        Some(Self::Move {
            volume: volume.to_owned(),
            from: name(from),
            to: name(to),
            service,
            mirrors: mirrors.iter().map(&name).collect(),
        })
    }

    /// The lines a person reads: [`Self::line`], then for a Move the Service it starts and
    /// each setting its digest covers, as a deploy's approval shows them.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let started = match self {
            Self::Resize { .. }
            | Self::Mirror { .. }
            | Self::Restore { .. }
            | Self::MirrorRemoval { .. } => Vec::new(),
            Self::Move { service, .. } => service.lines(),
        };
        std::iter::once(self.line()).chain(started).collect()
    }

    /// One line a person reads: the Volume, both Servers, and that later syncs need no
    /// approval, or what stops and starts where. A resize names the Volume, its size, every
    /// Server and whether a copy may shrink. A Restore names the Server, the snapshot and
    /// the time after which writes are lost, and a removal the copy it deletes and what is
    /// left writing. The Volume and Server names are relayed, so a resize's, a Restore's and
    /// a removal's show through [`Shown`].
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Resize {
                volume,
                servers,
                size,
                shrink,
            } => {
                let servers = Shown::list(servers.iter().map(|server| Shown::text(server)));
                let change = Shown::words("Approved: set ")
                    .and_text(volume)
                    .and_words(" to ")
                    .and_text(size)
                    .and_words(" on ")
                    .and(servers);
                change
                    .and_words(if *shrink {
                        ", lowering any copy above it."
                    } else {
                        ". No copy shrinks."
                    })
                    .into()
            }
            Self::Mirror {
                volume,
                writer,
                mirror,
                replace,
            } => {
                let change = if *replace {
                    format!(
                        "copy {volume} from {writer} to {mirror} again, replacing the copy there"
                    )
                } else {
                    format!("mirror {volume} from {writer} to {mirror}")
                };
                format!(
                    "Approved: {change}. Later syncs run without asking until you remove the Mirror."
                )
            }
            Self::Move {
                volume,
                from,
                to,
                service,
                mirrors,
            } => {
                let mut line = format!(
                    "Approved: move {volume} from {from} to {to}. {name} stops on {from} for the last copy, then starts on {to}.",
                    name = service.service,
                );
                match mirrors.as_slice() {
                    [] => {}
                    [mirror] => {
                        let _ = write!(line, " The Mirror on {mirror} keeps syncing.");
                    }
                    several => {
                        let _ = write!(
                            line,
                            " The Mirrors on {} keep syncing.",
                            several.join(" and ")
                        );
                    }
                }
                line
            }
            Self::Restore {
                volume,
                server,
                at,
                lost_after,
            } => Shown::words("Approved: make the copy of ")
                .and_text(volume)
                .and_words(" on ")
                .and_text(server)
                .and_words(" its writer, at snapshot ")
                .and_text(&at.to_string())
                .and_words(". Writes to ")
                .and_text(volume)
                .and_words(" after ")
                .and_text(lost_after)
                .and_words(" are lost. Nothing starts until you deploy.")
                .into(),
            Self::MirrorRemoval {
                volume,
                server,
                role,
                writer,
            } => {
                let change = Shown::words("Approved: delete ")
                    .and_text(volume)
                    .and_words(match role {
                        SlotRole::Mirror => "'s mirror on ",
                        SlotRole::OldCopy => "'s old copy on ",
                    })
                    .and_text(server);
                match writer {
                    Some(writer) => change
                        .and_words(". ")
                        .and_text(writer)
                        .and_words(" keeps writing ")
                        .and_text(volume)
                        .and_words("."),
                    None => change
                        .and_words(". No Server writes ")
                        .and_text(volume)
                        .and_words(
                            ", so this may be its last copy, and its data is gone for good.",
                        ),
                }
                .into()
            }
        }
    }
}

/// What a Machine knows when it checks a Volume approval against one step.
#[derive(Clone, Debug)]
pub struct VolumeFacts<'a> {
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub machine: &'a MachineId,
    pub trusted: &'a TrustedKeys,
    pub volume: &'a DockerVolumeName,
    pub step: &'a VolumeStep,
    pub now: Timestamp,
    /// A handoff receipt this Machine already checked, which proves the Move started on its
    /// source inside the window, so a first step here after the window is still admitted.
    pub handed: Option<&'a HandoffReceipt>,
}

/// A Volume approval a Machine checked: its body, the Device that signed it, and the
/// digest of the body as signed, which the Machine's record keeps.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeApproved {
    pub body: VolumeApproval,
    pub by: SigningPublicKey,
    pub digest: StepDigest,
}

/// Check `approval` lets this Machine take `facts.step` now, signed by a Device this
/// Machine trusts. A browser key is refused even under a valid session grant, and so is an
/// envelope that rides a proposal.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a signature problem, a malformed body, a
/// signer that is not a trusted Device, a body naming another Cluster or Volume, a
/// workflow that does not name this Machine or does not permit the step, an invalid
/// lifetime, or an expired window. An ended operation is the Machine's record to refuse.
pub fn verify(
    approval: &Signed<VolumeApproval>,
    facts: &VolumeFacts<'_>,
) -> Result<VolumeApproved, AuthorityRefusal> {
    let approvers = approval.approvers(facts.trusted, facts.now)?;
    let body = approval.open()?;
    if approval.proposal.is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: "a proposal",
        });
    }
    let by = approvers
        .into_iter()
        .find_map(|approver| match approver {
            Approver::Device(key) if approver.signs_as(body.signer) => Some(key),
            Approver::Device(_) | Approver::Browser { .. } | Approver::Passkey(_) => None,
        })
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })?;
    let cluster = facts.cluster.ok_or(AuthorityRefusal::NoClusterIdentity)?;
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        ("a Volume", &body.volume == facts.volume),
        (field::MACHINES, body.workflow.distinct()),
        ("this Server", body.workflow.names(facts.machine)),
        (
            "a Volume step",
            body.workflow.permits(facts.machine, facts.step),
        ),
    ])?;
    let digest = StepDigest::of(&approval.body);
    match super::validity::verify(body.issued_at, body.expires_at, facts.now) {
        Err(AuthorityRefusal::Expired)
            if facts.handed.is_some_and(|receipt| {
                receipt.operation == body.operation
                    && receipt.approval == digest
                    && receipt.to == *facts.machine
            }) => {}
        checked => checked?,
    }
    Ok(VolumeApproved { body, by, digest })
}

/// What a Move's source signs when it gives the Volume up: its one decision for the
/// operation, made after its cut. It names everything its target checks before it becomes
/// the Writer, and no time, since the Move it ends may finish at any later moment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct HandoffReceipt {
    /// The Cluster's Sealing public key, which the signing key is derived beside.
    pub cluster: SealingPublicKey,
    pub operation: OperationId,
    /// The digest of the Move approval's body as signed.
    pub approval: StepDigest,
    pub volume: DockerVolumeName,
    pub from: MachineId,
    pub to: MachineId,
    /// The cut's snapshot, which the target becomes the Writer at.
    pub at: SnapshotGuid,
    pub snapshot: SnapshotName,
    /// The source's Writer epoch when it cut.
    pub from_epoch: WriterEpoch,
    /// The epoch the target takes.
    pub epoch: WriterEpoch,
}

impl MachineStatement for HandoffReceipt {
    const KIND: &'static str = "volume-handoff";
}

/// What a Move's target knows when it checks a handoff receipt against one step.
#[derive(Clone, Copy, Debug)]
pub struct HandoffFacts<'a> {
    /// The receipt key this Machine derives from the Sealing Key it holds.
    pub key: SigningPublicKey,
    pub cluster: SealingPublicKey,
    pub machine: &'a MachineId,
    pub operation: OperationId,
    pub approval: StepDigest,
    pub volume: &'a DockerVolumeName,
    /// The snapshot the step lands on: a promotion's guid, a pull's name.
    pub at: Handed<'a>,
}

/// The snapshot a step under a receipt lands on.
#[derive(Clone, Copy, Debug)]
pub enum Handed<'a> {
    /// A promotion to the Writer at a snapshot and epoch.
    Writer {
        at: &'a SnapshotGuid,
        epoch: WriterEpoch,
    },
    /// A pull of a snapshot by name.
    Pull { snapshot: &'a SnapshotName },
}

/// Check `receipt` is the Cluster's statement that this Move's source handed this Volume
/// to this Machine at the step's snapshot.
///
/// # Errors
/// Returns the signature's refusal from [`MachineSigned::verify`], and
/// [`AuthorityRefusal::Mismatch`] naming the first field the step does not match.
pub fn verify_receipt(
    receipt: &MachineSigned<HandoffReceipt>,
    facts: &HandoffFacts<'_>,
) -> Result<HandoffReceipt, AuthorityRefusal> {
    let body = receipt.verify(facts.key)?;
    let (snapshot, epoch) = match facts.at {
        Handed::Writer { at, epoch } => (body.at == *at, body.epoch == epoch),
        Handed::Pull { snapshot } => (body.snapshot == *snapshot, true),
    };
    first_mismatch([
        (field::CLUSTER, body.cluster == facts.cluster),
        ("an operation", body.operation == facts.operation),
        ("an approval", body.approval == facts.approval),
        ("a Volume", body.volume == *facts.volume),
        ("this Server", body.to == *facts.machine),
        ("a snapshot", snapshot),
        ("an epoch", epoch && body.epoch > body.from_epoch),
    ])?;
    Ok(body)
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
