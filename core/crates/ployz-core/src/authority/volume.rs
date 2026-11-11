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
    keys::{Approver, Purpose, Signed, SigningPublicKey, Statement},
    proposal::StepDigest,
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::SignerKey,
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
                            NoneToMirror | MirrorToMirror | MirrorToWriter | WriterToWriter
                        ))
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

/// What a person approved in a Volume approval, as a client shows it, computed from the
/// statement it signs. The Volume and each Server go by the names the client knows them by.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovedVolumeChange {
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
            VolumeWorkflow::Move { .. }
            | VolumeWorkflow::MirrorRemoval { .. }
            | VolumeWorkflow::Restore { .. }
            | VolumeWorkflow::Resize { .. }
            | VolumeWorkflow::Seed { .. } => None,
        }
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

    /// One line a person reads: the Volume, both Servers, and that later syncs need no
    /// approval.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
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
                    "Approved: move {volume} from {from} to {to}. {name} stops on {from} for the last copy, then starts on {to}. {started}",
                    name = service.service,
                    started = service.line(),
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
