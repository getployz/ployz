//! The approval a person gives to take a Namespace or a Server off a Cluster: what goes,
//! where, and which Volumes lose their data.
//!
//! A teardown only removes. A Machine admits under one a step that stops, holds, takes out
//! of routing or removes a Container of a named Namespace, the removal of a named Docker
//! Volume, or a named Server's `Leave` and the others' `Forget` of it. It never admits a
//! creation or a start, so it can't run code, and it names each Volume whose data goes, so
//! the loss a person confirmed is the loss every Machine allows.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::{self, Timestamp},
    keys::{Approver, Purpose, Signed, Statement},
    preview::{PreviewRule, is_preview_namespace},
    private_access::key_approval_challenge,
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::{RestartStep, SignerKey},
    rule::verify_signature,
    shown::Shown,
    trusted_key::{PasskeyAssertion, TrustedKeys},
};
use crate::{DockerVolumeId, DockerVolumeName, MachineId, Namespace, SealingPublicKey};

/// A person approves taking a Namespace or a Server off one Cluster, for a bounded
/// lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Teardown {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    pub scope: TeardownScope,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// A Device, or `"passkey"` for a passkey's own prompt. A browser key is refused.
    pub signer: SignerKey,
}

impl Statement for Teardown {
    const PURPOSE: Purpose = Purpose::Workflow;
}

/// What a teardown takes off the Cluster.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TeardownScope {
    /// Every Container of `namespaces` on `machines` stops and goes, and each of `volumes`
    /// is deleted with its data. A Deploy that deletes Volumes signs one with no Machines,
    /// which takes no Container off.
    Namespace {
        /// Sorted and distinct.
        #[serde(deserialize_with = "canonical::sorted_distinct")]
        namespaces: Vec<Namespace>,
        /// Sorted and distinct.
        #[serde(deserialize_with = "canonical::sorted_distinct")]
        machines: Vec<MachineId>,
        /// Each a Docker Volume of one of `namespaces`. Sorted and distinct.
        #[serde(deserialize_with = "canonical::sorted_distinct")]
        volumes: Vec<DockerVolumeId>,
    },
    /// `leaving` resets and leaves the Cluster, and each of `forgetting` drops it. The
    /// Cluster loses `volumes`, the copies `leaving` holds.
    Server {
        leaving: MachineId,
        /// Sorted and distinct.
        #[serde(deserialize_with = "canonical::sorted_distinct")]
        forgetting: Vec<MachineId>,
        /// Sorted and distinct.
        #[serde(deserialize_with = "canonical::sorted_distinct")]
        volumes: Vec<DockerVolumeName>,
    },
}

/// One request a Machine is asked to take under a teardown.
#[derive(Clone, Copy, Debug)]
pub enum TeardownStep<'a> {
    /// A lifecycle step on an existing Container of `namespace`.
    Container {
        namespace: &'a Namespace,
        step: RestartStep,
    },
    /// Deleting a Docker Volume on this Machine.
    Volume(&'a DockerVolumeName),
    /// This Machine resets and leaves.
    Leave,
    /// This Machine drops another from its Cluster.
    Forget(&'a MachineId),
}

impl TeardownScope {
    /// Whether this scope lets `machine` take `step`. A start or a creation is never one.
    #[must_use]
    pub fn permits(&self, machine: &MachineId, step: TeardownStep<'_>) -> bool {
        match (self, step) {
            (
                Self::Namespace {
                    namespaces,
                    machines,
                    ..
                },
                TeardownStep::Container { namespace, step },
            ) => {
                step != RestartStep::Start
                    && namespaces.binary_search(namespace).is_ok()
                    && machines.binary_search(machine).is_ok()
            }
            (Self::Namespace { volumes, .. }, TeardownStep::Volume(name)) => volumes
                .iter()
                .any(|volume| &volume.machine_id == machine && &volume.name == name),
            (Self::Server { leaving, .. }, TeardownStep::Leave) => leaving == machine,
            (
                Self::Server {
                    leaving,
                    forgetting,
                    ..
                },
                TeardownStep::Forget(forgotten),
            ) => {
                forgotten == leaving
                    && machine != leaving
                    && forgetting.binary_search(machine).is_ok()
            }
            _ => false,
        }
    }

    /// Each Volume belongs to a Namespace the teardown names, and a leaving Server is not
    /// one that forgets it.
    fn consistent(&self) -> bool {
        match self {
            Self::Namespace {
                namespaces,
                volumes,
                ..
            } => volumes.iter().all(|volume| {
                namespaces
                    .iter()
                    .any(|namespace| owns_volume(namespace, &volume.name))
            }),
            Self::Server {
                leaving,
                forgetting,
                ..
            } => !forgetting.contains(leaving),
        }
    }
}

/// Whether `volume` is a Docker Volume `namespace` declared: `<namespace>_<name>`.
#[must_use]
pub fn owns_volume(namespace: &Namespace, volume: &DockerVolumeName) -> bool {
    volume
        .as_str()
        .strip_prefix(namespace.as_str())
        .is_some_and(|rest| rest.starts_with('_'))
}

/// What a teardown step travels with: a teardown signed by a Device, or answered by a
/// passkey's own prompt over its challenge as a Private Access approval is, or the preview
/// rule whose own preview it closes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TeardownApproval {
    Signed(Signed<Teardown>),
    Passkey {
        body: String,
        assertion: PasskeyAssertion,
    },
    PreviewRule(Signed<PreviewRule>),
}

impl TeardownApproval {
    /// The teardown this approval carries, read without checking who signed it. `None`
    /// for a preview rule's close.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Malformed`] when the body has no canonical reading.
    pub fn body(&self) -> Result<Option<Teardown>, AuthorityRefusal> {
        match self {
            Self::Signed(signed) => parse(&signed.body).map(Some),
            Self::Passkey { body, .. } => parse(body).map(Some),
            Self::PreviewRule(_) => Ok(None),
        }
    }
}

fn parse(text: &str) -> Result<Teardown, AuthorityRefusal> {
    canonical::from_canonical(text).map_err(|error| AuthorityRefusal::Malformed {
        purpose: Purpose::Workflow,
        problem: error.to_string(),
    })
}

/// What a client knows when it drafts a teardown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TeardownTerms {
    pub cluster: SealingPublicKey,
    pub scope: TeardownScope,
    pub issued_at: Timestamp,
    pub signer: SignerKey,
}

impl Teardown {
    /// The teardown of `terms`, its lists sorted and distinct, for the longest lifetime a
    /// Machine takes.
    #[must_use]
    pub fn drafted(terms: TeardownTerms) -> Self {
        let scope = match terms.scope {
            TeardownScope::Namespace {
                namespaces,
                machines,
                volumes,
            } => TeardownScope::Namespace {
                namespaces: sorted(namespaces),
                machines: sorted(machines),
                volumes: sorted(volumes),
            },
            TeardownScope::Server {
                leaving,
                forgetting,
                volumes,
            } => TeardownScope::Server {
                leaving,
                forgetting: sorted(forgetting),
                volumes: sorted(volumes),
            },
        };
        Self {
            cluster: terms.cluster,
            scope,
            issued_at: terms.issued_at,
            expires_at: terms.issued_at.plus(super::validity::MAX_LIFETIME_MILLIS),
            signer: terms.signer,
        }
    }

    /// The lines a person reads before they approve, from this body. `name` gives each
    /// Server the name the client knows it by. Every name is relayed, so it shows through
    /// [`Shown`].
    #[must_use]
    pub fn lines(&self, name: impl Fn(&MachineId) -> String) -> Vec<String> {
        let servers = |machines: &[MachineId]| {
            Shown::list(machines.iter().map(|machine| Shown::text(&name(machine))))
        };
        match &self.scope {
            TeardownScope::Namespace {
                namespaces,
                machines,
                volumes,
            } => {
                let mut lines = Vec::new();
                if !machines.is_empty() {
                    lines.push(
                        Shown::words("Approved: stop and remove every service of ")
                            .and(Shown::list(
                                namespaces
                                    .iter()
                                    .map(|namespace| Shown::text(namespace.as_str())),
                            ))
                            .and_words(" on ")
                            .and(servers(machines))
                            .and_words(".")
                            .into(),
                    );
                }
                lines.extend(volumes.iter().map(|volume| {
                    Shown::words("Approved: delete volume ")
                        .and_text(volume.name.as_str())
                        .and_words(" on ")
                        .and_text(&name(&volume.machine_id))
                        .and_words(". Its data is gone for good.")
                        .into()
                }));
                if volumes.is_empty() {
                    lines.push("No volume data is deleted.".to_owned());
                }
                lines
            }
            TeardownScope::Server {
                leaving,
                forgetting,
                volumes,
            } => {
                let mut lines = vec![
                    Shown::words("Approved: remove ")
                        .and_text(&name(leaving))
                        .and_words(" from the Cluster. Its services stop there.")
                        .into(),
                ];
                if !forgetting.is_empty() {
                    lines.push(
                        Shown::words("Approved: ")
                            .and(servers(forgetting))
                            .and_words(" forget ")
                            .and_text(&name(leaving))
                            .and_words(".")
                            .into(),
                    );
                }
                lines.push(match volumes.as_slice() {
                    [] => Shown::words("It holds no volume copies.").into(),
                    volumes => Shown::words("The Cluster can no longer reach its copies of ")
                        .and(Shown::list(
                            volumes.iter().map(|volume| Shown::text(volume.as_str())),
                        ))
                        .and_words(".")
                        .into(),
                });
                lines
            }
        }
    }
}

fn sorted<T: Ord>(mut items: Vec<T>) -> Vec<T> {
    items.sort();
    items.dedup();
    items
}

/// What a Machine knows when it checks a teardown against one request.
#[derive(Clone, Debug)]
pub struct TeardownFacts<'a> {
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub machine: &'a MachineId,
    pub trusted: &'a TrustedKeys,
    pub step: TeardownStep<'a>,
    pub now: Timestamp,
}

/// Who approved a teardown step a Machine checked, and under what.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeardownApproved {
    pub by: Approver,
    /// The preview rule whose own preview the step closes; `None` under a teardown.
    pub preview_rule: Option<super::RuleId>,
}

/// Check `approval` lets this Machine take `facts.step` now: signed by a Device this
/// Machine trusts, answered by a trusted passkey's own prompt, or, for a preview's close,
/// the rule that made it as [`verify_preview_close`] checks. A browser key is refused
/// even under a valid session grant, since a grant lent for deploys says nothing about
/// deleting data, and so is an envelope that rides a proposal.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a signature problem, a malformed body, a
/// signer that is not a trusted Device or passkey, a body naming another Cluster, a scope
/// whose Volumes lie outside its Namespaces, a step the scope does not permit on this
/// Machine, an invalid lifetime, or an expired window.
pub fn verify(
    approval: &TeardownApproval,
    facts: &TeardownFacts<'_>,
) -> Result<TeardownApproved, AuthorityRefusal> {
    let (by, body) = match approval {
        TeardownApproval::PreviewRule(rule) => {
            let (rule, by) = verify_preview_close(rule, facts)?;
            return Ok(TeardownApproved {
                by,
                preview_rule: Some(rule.rule),
            });
        }
        TeardownApproval::Passkey {
            body: text,
            assertion,
        } => {
            let passkey = facts.trusted.passkey_approval(
                Purpose::Workflow,
                assertion,
                &key_approval_challenge(text),
            )?;
            (Approver::Passkey(passkey.passkey.code()), parse(text)?)
        }
        TeardownApproval::Signed(signed) => {
            let approvers = signed.approvers(facts.trusted, facts.now)?;
            let body = signed.open()?;
            if signed.proposal.is_some() {
                return Err(AuthorityRefusal::Mismatch {
                    field: "a proposal",
                });
            }
            let by = approvers
                .into_iter()
                .find(|approver| {
                    matches!(approver, Approver::Device(_)) && approver.signs_as(body.signer)
                })
                .ok_or(AuthorityRefusal::Mismatch {
                    field: "the signer",
                })?;
            (by, body)
        }
    };
    if !by.signs_as(body.signer) {
        return Err(AuthorityRefusal::Mismatch {
            field: "the signer",
        });
    }
    let cluster = facts.cluster.ok_or(AuthorityRefusal::NoClusterIdentity)?;
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        ("a Volume", body.scope.consistent()),
        (
            step_field(facts.step),
            body.scope.permits(facts.machine, facts.step),
        ),
    ])?;
    super::validity::verify(body.issued_at, body.expires_at, facts.now)?;
    Ok(TeardownApproved {
        by,
        preview_rule: None,
    })
}

/// Check `rule` lets this Machine take `facts.step` to close one of the rule's own
/// previews, with no new prompt: the rule signed by a trusted Device or passkey for this
/// Cluster, and the step one that starts nothing in a Namespace the rule derives, or the
/// removal of a Docker Volume of one. A preview's Namespace is `pv-` and the first 16 hex
/// digits of the rule's ID, which the approver's client chose at random, so it holds only
/// what this rule created. Any generation closes them, an ended one too, and the Machines
/// it names don't bound it: a preview a past generation placed still closes.
///
/// # Errors
/// Returns the rule's signature refusal from [`verify_signature`], a body naming another
/// Cluster, and [`AuthorityRefusal::Mismatch`] naming the step for anything but a close of
/// this rule's preview.
pub fn verify_preview_close(
    rule: &Signed<PreviewRule>,
    facts: &TeardownFacts<'_>,
) -> Result<(PreviewRule, Approver), AuthorityRefusal> {
    let (body, by) = verify_signature(rule, facts.trusted, facts.now)?;
    let cluster = facts.cluster.ok_or(AuthorityRefusal::NoClusterIdentity)?;
    let derived = |namespace: &Namespace| {
        is_preview_namespace(namespace)
            && namespace
                .as_str()
                .strip_prefix("pv-")
                .zip(body.rule.to_string().get(..16))
                .is_some_and(|(rest, prefix)| rest.starts_with(prefix))
    };
    let closes = match facts.step {
        TeardownStep::Container { namespace, step } => {
            step != RestartStep::Start && derived(namespace)
        }
        TeardownStep::Volume(volume) => volume
            .as_str()
            .split_once('_')
            .and_then(|(namespace, _)| Namespace::parse(namespace).ok())
            .is_some_and(|namespace| derived(&namespace)),
        TeardownStep::Leave | TeardownStep::Forget(_) => false,
    };
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        (step_field(facts.step), closes),
    ])?;
    Ok((body, by))
}

/// The field a refusal names when the scope does not permit `step`.
fn step_field(step: TeardownStep<'_>) -> &'static str {
    match step {
        TeardownStep::Container { .. } => "a Namespace on this Server",
        TeardownStep::Volume(_) => "a Volume on this Server",
        TeardownStep::Leave => "this Server",
        TeardownStep::Forget(_) => "a Server to forget",
    }
}

#[cfg(test)]
#[path = "teardown_tests.rs"]
mod tests;
