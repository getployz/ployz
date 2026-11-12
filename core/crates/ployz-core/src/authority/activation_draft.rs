//! An activation as the dashboard drafts it for a passkey's own prompts: the starting set
//! over the Servers it lists, then the rotation naming the recipient key a Server generated.
//! Each draft is the canonical body the passkey answers and the lines the person reads,
//! rendered from that body as a Server parses it, never from what the client sent. The
//! browser runs this as WASM.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp},
    restart::SignerKey,
    rotation::SealingRotation,
    shown::Shown,
    starting_set::{Member, StartingSet},
    validity::MAX_LIFETIME_MILLIS,
};
use crate::{MachineId, OperationId};

/// What a client knows when it drafts a starting set. It keeps no key added while
/// enforcement was off: a Cluster with any is activated from a Device instead.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StartingSetTerms {
    pub operation: OperationId,
    pub members: Vec<Member>,
    pub issued_at: Timestamp,
    pub signer: SignerKey,
}

/// A statement's canonical body and the lines a person reads before answering it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ActivationDraft {
    pub body: String,
    pub lines: Vec<String>,
}

impl StartingSet {
    /// The starting set of `terms`, its Members sorted and distinct, for the longest
    /// lifetime a Machine takes, with the lines a person reads. `name` gives each Server
    /// the name Cloud knows it by, shown beside what the body names.
    ///
    /// # Errors
    /// Returns [`CanonicalError`] for a body with no canonical form.
    pub fn draft(
        terms: StartingSetTerms,
        name: impl Fn(&MachineId) -> String,
    ) -> Result<ActivationDraft, CanonicalError> {
        let set = Self {
            operation: terms.operation,
            members: sorted(terms.members),
            issued_at: terms.issued_at,
            expires_at: terms.issued_at.plus(MAX_LIFETIME_MILLIS),
            signer: terms.signer,
            kept_keys: Vec::new(),
        };
        let body = canonical::to_canonical(&set)?;
        let signed: Self = canonical::from_canonical(&body)?;
        Ok(ActivationDraft {
            lines: signed.lines(name),
            body,
        })
    }

    /// What a person approves when they answer this set.
    #[must_use]
    pub fn lines(&self, name: impl Fn(&MachineId) -> String) -> Vec<String> {
        let mut lines = vec![
            Shown::words("Approved: turn on device enforcement for good on ")
                .and_words(if self.members.len() == 1 {
                    "this server"
                } else {
                    "these servers"
                })
                .and_words(". There's no off.")
                .into(),
        ];
        lines.extend(self.members.iter().map(|member| member_line(member, &name)));
        lines.push(match self.kept_keys.as_slice() {
            [] => Shown::words(
                "Approved: remove every device and passkey added while enforcement was off.",
            )
            .into(),
            kept => Shown::words("Approved: keep these keys added while enforcement was off: ")
                .and(Shown::list(
                    kept.iter().map(|code| Shown::text(&code.to_string())),
                ))
                .and_words(".")
                .into(),
        });
        lines
    }
}

impl SealingRotation {
    /// `self` with its Members sorted and distinct, and the lines a person reads.
    ///
    /// # Errors
    /// Returns [`CanonicalError`] for a body with no canonical form.
    pub fn draft(
        self,
        name: impl Fn(&MachineId) -> String,
    ) -> Result<ActivationDraft, CanonicalError> {
        let rotation = Self {
            members: sorted(self.members),
            ..self
        };
        let body = canonical::to_canonical(&rotation)?;
        let signed: Self = canonical::from_canonical(&body)?;
        Ok(ActivationDraft {
            lines: signed.lines(name),
            body,
        })
    }

    /// What a person approves when they answer this rotation.
    #[must_use]
    pub fn lines(&self, name: impl Fn(&MachineId) -> String) -> Vec<String> {
        let mut lines = vec![
            Shown::words("Approved: seal new values to key ")
                .and_text(&self.recipient.to_string())
                .and_words(". Only these servers get it:")
                .into(),
        ];
        lines.extend(self.members.iter().map(|member| member_line(member, &name)));
        lines.push(
            Shown::words("  cluster ")
                .and_text(&self.cluster.to_string())
                .into(),
        );
        lines.push(
            Shown::words("  activation ")
                .and_text(&self.operation.to_string())
                .into(),
        );
        lines
    }
}

/// One Member as a person checks it: the name Cloud gives it, then the key fingerprint
/// `ployz status` on it shows, its subnet and its ID, all from the body.
fn member_line(member: &Member, name: &impl Fn(&MachineId) -> String) -> String {
    Shown::words("  ")
        .and_text(&name(&member.id))
        .and_words(", key ")
        .and_text(&member.public_key.fingerprint())
        .and_words(", subnet ")
        .and_text(&member.subnet.to_string())
        .and_words(", ID ")
        .and_text(&member.id.to_string())
        .into()
}

fn sorted(mut members: Vec<Member>) -> Vec<Member> {
    members.sort_by_key(|member| member.id);
    members.dedup();
    members
}

/// The name each Server is shown by, or its ID when Cloud gave none.
pub fn named(servers: &BTreeMap<MachineId, String>) -> impl Fn(&MachineId) -> String + '_ {
    |id| servers.get(id).cloned().unwrap_or_else(|| id.to_string())
}

#[cfg(test)]
#[path = "activation_draft_tests.rs"]
mod tests;
