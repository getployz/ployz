//! The approval a person's Device signs for one Volume workflow, the steps each workflow
//! lets each Machine take, and the checks a Machine runs before it admits the first step.
//!
//! A workflow is a graph, not a list of routes: [`VolumeWorkflow::permits`] answers for
//! one Machine and one step. A Machine keeps its own record of the operations it admitted,
//! so a later step of an open operation needs no fresh window and an ended one never runs
//! again. That record is the Machine's, not this module's.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    deploy::ApprovedService,
    keys::{Approver, Purpose, Signed, SigningPublicKey, Statement},
    proposal::StepDigest,
    refusal::AuthorityRefusal,
    restart::{SignerKey, ordered_machines},
    trusted_key::TrustedKeys,
};
use crate::{
    CopyTransition, DockerVolumeName, MachineId, Namespace, OperationId,
    ProvisionedVolumeMaximumBytes, SealingPublicKey, SlotRole, SnapshotGuid,
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
        #[serde(deserialize_with = "ordered_machines")]
        mirrors: Vec<MachineId>,
    },
    /// `mirror` keeps a copy of the Writer on `writer`, and every later sync of it, until
    /// the Mirror is removed. With `replace` it first destroys the copy it holds.
    Mirror {
        writer: MachineId,
        mirror: MachineId,
        replace: bool,
    },
    /// `mirror` destroys its copy, held in `role`. `last_copy` says no other copy remains
    /// beside the Writer's.
    MirrorRemoval {
        mirror: MachineId,
        writer: MachineId,
        role: SlotRole,
        last_copy: bool,
    },
    /// `machine` promotes its copy at `at` with no start. Writes after `loses_after` are
    /// lost.
    Restore {
        machine: MachineId,
        at: SnapshotGuid,
        loses_after: Timestamp,
    },
    /// Each of `machines` bounds the Volume at `maximum_bytes`, below the bound it holds
    /// only when `shrink`.
    Resize {
        /// Sorted and distinct.
        #[serde(deserialize_with = "ordered_machines")]
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
                        WriterToSnapshot | WriterToMirror | WriterToWriter
                    ))
                    || (machine == to
                        && matches!(transition, NoneToMirror | MirrorToMirror | MirrorToWriter))
                    || (mirrors.contains(machine) && *transition == MirrorToMirror)
            }
            (Self::Move { from, to, .. }, VolumeStep::Start) => machine == from || machine == to,
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
                machine == owner && *transition == MirrorToWriter
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
            ) => {
                machines.contains(machine) && wanted == maximum_bytes && (*shrink || !allow_shrink)
            }
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

    /// Whether this workflow changes the role of `machine`'s copy, or destroys it. A
    /// Machine holds at most one open owner of a Volume's copy.
    #[must_use]
    pub fn owns(&self, machine: &MachineId) -> bool {
        match self {
            Self::Move { from, to, .. } => machine == from || machine == to,
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
            Self::Mirror { writer, mirror, .. } | Self::MirrorRemoval { mirror, writer, .. } => {
                machine == writer || machine == mirror
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
            Self::Mirror { writer, mirror, .. } | Self::MirrorRemoval { mirror, writer, .. } => {
                writer != mirror
            }
            Self::Restore { .. } | Self::Resize { .. } | Self::Seed { .. } => true,
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
            Approver::Device(_) | Approver::Browser { .. } => None,
        })
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })?;
    let cluster = facts.cluster.ok_or(AuthorityRefusal::NoClusterIdentity)?;
    let checks = [
        ("a Cluster", body.cluster == cluster),
        ("a Volume", &body.volume == facts.volume),
        ("Machines", body.workflow.distinct()),
        ("this Server", body.workflow.names(facts.machine)),
        (
            "a Volume step",
            body.workflow.permits(facts.machine, facts.step),
        ),
    ];
    if let Some((field, _)) = checks.iter().find(|(_, holds)| !holds) {
        return Err(AuthorityRefusal::Mismatch { field });
    }
    super::validity::verify(body.issued_at, body.expires_at, facts.now)?;
    Ok(VolumeApproved {
        body,
        by,
        digest: StepDigest::of(&approval.body),
    })
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
