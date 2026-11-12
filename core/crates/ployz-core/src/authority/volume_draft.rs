//! A Volume approval as a client drafts it before a person approves it: the canonical body
//! to sign or answer as it is, and every line the person reads, rendered from that body.
//! The browser runs this as WASM, so the dashboard shows and signs what the CLI and the
//! Servers read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::{self, Timestamp},
    deploy::{ApprovedChange, content_addressed},
    restart::SignerKey,
    shown::Shown,
    validity::MAX_LIFETIME_MILLIS,
    volume::{ApprovedVolumeChange, VolumeApproval, VolumeWorkflow},
};
use crate::{
    DockerVolumeName, MachineId, Namespace, OperationId, ProvisionedVolumeMaximumBytes, PullPolicy,
    ResolvedServiceSpec, SealingPublicKey, SlotRole, SnapshotGuid,
};

/// The workflow a client drafts. A Move names the spec its Service runs with on the source
/// now, and the draft derives the one it starts with on the target; every other workflow
/// is drafted as it is signed.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftWorkflow {
    Move {
        from: MachineId,
        to: MachineId,
        mirrors: Vec<MachineId>,
        /// The spec the Service's one Container on `from` runs with, as its Server keeps it.
        running: Box<ResolvedServiceSpec>,
        /// The size the Store records for the Volume, which the target's copy takes.
        #[serde(default)]
        #[ts(optional)]
        recorded: Option<ProvisionedVolumeMaximumBytes>,
    },
    Mirror {
        writer: MachineId,
        mirror: MachineId,
        replace: bool,
    },
    MirrorRemoval {
        mirror: MachineId,
        #[serde(default)]
        #[ts(optional)]
        writer: Option<MachineId>,
        role: SlotRole,
    },
    Restore {
        machine: MachineId,
        at: SnapshotGuid,
        #[serde(default)]
        #[ts(optional)]
        loses_after: Option<Timestamp>,
    },
    Resize {
        machines: Vec<MachineId>,
        maximum_bytes: ProvisionedVolumeMaximumBytes,
        shrink: bool,
    },
    Seed {
        machine: MachineId,
        source: DockerVolumeName,
        at: SnapshotGuid,
    },
}

/// What a client knows when it drafts a Volume approval: the Cluster, the Volume by its
/// Docker name and by the name the person knows it by, each Server the workflow names by
/// the name it reported, the workflow, when, and who answers it.
#[derive(Clone, Debug, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumeTerms {
    pub cluster: SealingPublicKey,
    pub operation: OperationId,
    pub namespace: Namespace,
    pub volume: DockerVolumeName,
    /// The Volume's name in its Environment. Relayed, so it shows quoted when it must.
    pub name: String,
    /// The name each Server reported. Relayed, so each shows quoted when it must.
    pub servers: BTreeMap<MachineId, String>,
    pub workflow: DraftWorkflow,
    pub issued_at: Timestamp,
    pub signer: SignerKey,
}

/// A drafted approval: the canonical `body` to sign or answer as it is, the `lines` a
/// person reads first, and for a Move the spec the target starts, which the request
/// carries and the target hashes against the body's digest.
#[derive(Clone, Debug, Serialize, TS)]
pub struct VolumeDraft {
    pub body: String,
    pub lines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service: Option<Box<ResolvedServiceSpec>>,
}

/// Why a Volume approval can't be drafted, in words a person reads as they are.
#[derive(Debug, thiserror::Error)]
pub enum VolumeDraftError {
    #[error(
        "{service} runs {image}, which isn't pinned by digest, so it can't be approved to start on another server. Deploy it again, then move the volume."
    )]
    Unpinned { service: String, image: String },
    #[error("the approval has no canonical form: {0}")]
    Canonical(#[from] canonical::CanonicalError),
}

impl VolumeTerms {
    /// The approval these terms describe, for the longest lifetime a Machine takes, and
    /// the lines a person reads, rendered from the canonical body itself.
    ///
    /// # Errors
    /// Refuses a Move whose Service runs an image not pinned by digest, and a body with no
    /// canonical form.
    pub fn draft(self) -> Result<VolumeDraft, VolumeDraftError> {
        let (workflow, start) = match self.workflow {
            DraftWorkflow::Move {
                from,
                to,
                mut mirrors,
                running,
                recorded,
            } => {
                let start = move_start(*running, &self.volume, recorded)?;
                let [change] = <[ApprovedChange; 1]>::try_from(ApprovedChange::of(
                    &[start.to_requested()],
                    &self.namespace,
                    &BTreeMap::new(),
                )?)
                .expect("one spec makes one change");
                mirrors.sort();
                mirrors.dedup();
                let workflow = VolumeWorkflow::Move {
                    from,
                    to,
                    service: change.approved(),
                    mirrors,
                };
                (workflow, Some((start, change)))
            }
            DraftWorkflow::Mirror {
                writer,
                mirror,
                replace,
            } => (
                VolumeWorkflow::Mirror {
                    writer,
                    mirror,
                    replace,
                },
                None,
            ),
            DraftWorkflow::MirrorRemoval {
                mirror,
                writer,
                role,
            } => (
                VolumeWorkflow::MirrorRemoval {
                    mirror,
                    writer,
                    role,
                },
                None,
            ),
            DraftWorkflow::Restore {
                machine,
                at,
                loses_after,
            } => (
                VolumeWorkflow::Restore {
                    machine,
                    at,
                    loses_after,
                },
                None,
            ),
            DraftWorkflow::Resize {
                mut machines,
                maximum_bytes,
                shrink,
            } => {
                machines.sort();
                machines.dedup();
                (
                    VolumeWorkflow::Resize {
                        machines,
                        maximum_bytes,
                        shrink,
                    },
                    None,
                )
            }
            DraftWorkflow::Seed {
                machine,
                source,
                at,
            } => (
                VolumeWorkflow::Seed {
                    machine,
                    source,
                    at,
                },
                None,
            ),
        };
        let approval = VolumeApproval {
            cluster: self.cluster,
            operation: self.operation,
            namespace: self.namespace,
            volume: self.volume,
            workflow,
            issued_at: self.issued_at,
            expires_at: self.issued_at.plus(MAX_LIFETIME_MILLIS),
            signer: self.signer,
        };
        let body = canonical::to_canonical(&approval)?;
        // The lines come from the body as a Server reads it, not from the draft above.
        let signed: VolumeApproval = canonical::from_canonical(&body)?;
        let name = |id: &MachineId| {
            self.servers
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        };
        let (change, service) = match start {
            Some((start, change)) => (
                ApprovedVolumeChange::moving(&signed, &self.name, name, change),
                Some(Box::new(start)),
            ),
            None => (
                ApprovedVolumeChange::restoring(&signed, &self.name, name, Timestamp::utc)
                    .or_else(|| ApprovedVolumeChange::of(&signed, &self.name, name)),
                None,
            ),
        };
        let change = change.expect("a drafted workflow names its change");
        let lines = change
            .lines()
            .into_iter()
            .chain(terms(&signed, &self.servers))
            .collect();
        Ok(VolumeDraft {
            body,
            lines,
            service,
        })
    }
}

/// Every term of `signed` a person approves beyond the change itself, one per line, each
/// relayed value shown so it can't pass for another.
fn terms(signed: &VolumeApproval, servers: &BTreeMap<MachineId, String>) -> Vec<String> {
    let server = |id: &MachineId| match servers.get(id) {
        Some(name) => Shown::text(name)
            .and_words(" (")
            .and_text(id.as_str())
            .and_words(")"),
        None => Shown::text(id.as_str()),
    };
    let named = |words: &'static str, id: &MachineId| Shown::words(words).and(server(id));
    let listed = |words: &'static str, ids: &[MachineId]| {
        Shown::words(words).and(Shown::list(ids.iter().map(server)))
    };
    let mut lines = vec![
        Shown::words("  volume ").and_text(signed.volume.as_str()),
        Shown::words("  namespace ").and_text(signed.namespace.as_str()),
    ];
    match &signed.workflow {
        VolumeWorkflow::Move {
            from,
            to,
            service,
            mirrors,
        } => {
            lines.push(named("  from ", from));
            lines.push(named("  to ", to));
            lines.push(
                Shown::words("  service ")
                    .and_text(service.name.as_str())
                    .and_words(" ")
                    .and_text(&service.digest.to_string()),
            );
            lines.push(listed("  mirrors ", mirrors));
        }
        VolumeWorkflow::Mirror {
            writer,
            mirror,
            replace,
        } => {
            lines.push(named("  writer ", writer));
            lines.push(named("  mirror ", mirror));
            lines.push(Shown::words(if *replace {
                "  replace yes"
            } else {
                "  replace no"
            }));
        }
        VolumeWorkflow::MirrorRemoval {
            mirror,
            writer,
            role,
        } => {
            lines.push(named("  copy on ", mirror));
            lines.push(Shown::words(match role {
                SlotRole::Mirror => "  role mirror",
                SlotRole::OldCopy => "  role old copy",
            }));
            lines.push(match writer {
                Some(writer) => named("  writer ", writer),
                None => Shown::words("  writer none"),
            });
        }
        VolumeWorkflow::Restore {
            machine,
            at,
            loses_after,
        } => {
            lines.push(named("  server ", machine));
            lines.push(Shown::words("  snapshot ").and_text(&at.to_string()));
            lines.push(match loses_after {
                Some(after) => Shown::words("  loses writes after ").and_text(&after.utc()),
                None => Shown::words("  loses no writes"),
            });
        }
        VolumeWorkflow::Resize {
            machines,
            maximum_bytes,
            shrink,
        } => {
            lines.push(listed("  servers ", machines));
            lines.push(Shown::words("  size ").and_text(&maximum_bytes.get().to_string()));
            lines.push(Shown::words(if *shrink {
                "  shrink yes"
            } else {
                "  shrink no"
            }));
        }
        VolumeWorkflow::Seed {
            machine,
            source,
            at,
        } => {
            lines.push(named("  server ", machine));
            lines.push(Shown::words("  from ").and_text(source.as_str()));
            lines.push(Shown::words("  snapshot ").and_text(&at.to_string()));
        }
    }
    lines.push(Shown::words("  operation ").and_text(signed.operation.as_str()));
    lines.push(
        Shown::words("  valid ")
            .and_text(&signed.issued_at.utc())
            .and_words(" to ")
            .and_text(&signed.expires_at.utc()),
    );
    lines.push(Shown::words("  cluster ").and_text(&signed.cluster.to_string()));
    lines.into_iter().map(String::from).collect()
}

/// `running`, the spec a Move's Service runs with on its source, as the target starts it:
/// from the image already copied there, with `volume` at `recorded` when the Store records
/// a size.
///
/// # Errors
/// Refuses an image not pinned by digest, which no Server starts under approval.
pub fn move_start(
    running: ResolvedServiceSpec,
    volume: &DockerVolumeName,
    recorded: Option<ProvisionedVolumeMaximumBytes>,
) -> Result<ResolvedServiceSpec, VolumeDraftError> {
    if !content_addressed(&running.container.image) {
        return Err(VolumeDraftError::Unpinned {
            service: running.name.to_string(),
            image: running.container.image,
        });
    }
    let mut spec = serde_json::to_value(&running).expect("a resolved spec serializes to JSON");
    if let Some(recorded) = recorded
        && let Some(volumes) = spec
            .get_mut("volumes")
            .and_then(|volumes| volumes.as_array_mut())
    {
        for mounted in volumes {
            let source = &mut mounted["source"];
            if source["kind"] == "provisioned" && source["name"] == volume.as_str() {
                source["maximum_bytes"] = serde_json::json!(recorded);
            }
        }
    }
    let mut spec: ResolvedServiceSpec =
        serde_json::from_value(spec).expect("a resolved spec with a recorded size parses");
    spec.container.pull_policy = PullPolicy::Never;
    Ok(spec)
}

#[cfg(test)]
#[path = "volume_draft_tests.rs"]
mod tests;
