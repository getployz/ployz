//! A person's approval to restore or empty named entries of one Server's Trash.
//!
//! The approval names the Cluster, the one Machine whose Trash it reaches, what it does
//! and every entry it does it to, for a bounded lifetime. A Machine takes an Operator's
//! restore or empty only with an approval that names it and the very entries the request
//! names, so Cloud can carry it to no other Machine, entry or action. A Trash ID is never
//! minted twice, so an approval sent again within its window reaches nothing it didn't
//! already: a restored or emptied entry is gone, and a restore already pinned replays.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    keys::{Approver, Purpose, Signed, SignerKey, Statement},
    open::the_signer,
    refusal::{AuthorityRefusal, MismatchField, first_mismatch},
    scope::Ask,
    trusted_key::TrustedKeys,
    validity::{self, MAX_LIFETIME_MILLIS},
};
use crate::{EmptyTrashRequest, MachineId, RestoreTrashRequest, SealingPublicKey, TrashId};

/// What an approval lets the Machine do to its entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TrashAction {
    /// Put each entry's data back, as `ployz volume trash restore` does.
    Restore,
    /// Destroy each entry for good.
    Empty,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TrashApproval {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    /// The one Machine whose Trash this reaches.
    pub machine: MachineId,
    pub action: TrashAction,
    /// Every entry it reaches, sorted, each once.
    pub ids: Vec<TrashId>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// A Device, or `"passkey"` for a passkey's own prompt.
    pub signer: SignerKey,
}

impl Statement for TrashApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
    const TAKES_ASSERTION: bool = true;
}

/// What a client knows when it drafts an approval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TrashTerms {
    pub cluster: SealingPublicKey,
    pub machine: MachineId,
    pub action: TrashAction,
    pub ids: Vec<TrashId>,
    pub issued_at: Timestamp,
    pub signer: SignerKey,
}

impl TrashApproval {
    /// The approval of `terms`, its entries sorted and each once, for the longest lifetime
    /// a Machine takes.
    #[must_use]
    pub fn drafted(terms: &TrashTerms) -> Self {
        let mut ids = terms.ids.clone();
        ids.sort_unstable();
        ids.dedup();
        Self {
            cluster: terms.cluster,
            machine: terms.machine,
            action: terms.action,
            ids,
            issued_at: terms.issued_at,
            expires_at: terms.issued_at.plus(MAX_LIFETIME_MILLIS),
            signer: terms.signer,
        }
    }
}

/// What a Machine knows when it checks a Trash approval.
#[derive(Clone, Copy, Debug)]
pub struct TrashFacts<'a> {
    pub trusted: &'a TrustedKeys,
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub machine: MachineId,
    pub now: Timestamp,
}

/// What an Operator's Trash request asks of an approval.
#[derive(Clone, Copy, Debug)]
pub enum TrashAsk<'a> {
    /// One entry restored, in either of its calls.
    Restore(TrashId),
    /// Exactly these entries emptied, never the whole Trash.
    Empty { ids: &'a [TrashId], bulk: bool },
}

impl<'a> From<&'a RestoreTrashRequest> for TrashAsk<'a> {
    fn from(request: &'a RestoreTrashRequest) -> Self {
        Self::Restore(request.id)
    }
}

impl<'a> From<&'a EmptyTrashRequest> for TrashAsk<'a> {
    fn from(request: &'a EmptyTrashRequest) -> Self {
        Self::Empty {
            ids: &request.ids,
            bulk: request.bulk,
        }
    }
}

/// Check `approval` lets this Machine do what `ask` asks now: signed by a Device this
/// Machine trusts or answered by a trusted passkey's own prompt, alone and not in a
/// bundle, naming this Cluster and this Machine, the action asked, and the entries asked.
/// A restore names one of its entries, since each restore is its own request; an empty
/// names exactly its entries, in any order, and never asks for the whole Trash.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: an unsigned or badly signed approval, a
/// malformed body, an approver that isn't the signer the body names or whose kind doesn't
/// cover the Trash, a body that rides a bundle or names another Cluster, Machine, action
/// or set of entries, an invalid lifetime, or an expired window.
pub fn verify(
    approval: &Signed<TrashApproval>,
    ask: TrashAsk<'_>,
    facts: &TrashFacts<'_>,
) -> Result<Approver, AuthorityRefusal> {
    let approvers = approval.approvers(facts.trusted)?;
    if approval.proposal.is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Proposal,
        });
    }
    let body = approval.open()?;
    let by = the_signer(approvers, body.signer)?;
    facts.trusted.covers(&by, Ask::Trash)?;
    let cluster = facts.cluster.ok_or(AuthorityRefusal::NoClusterIdentity)?;
    let (action, entries) = match ask {
        TrashAsk::Restore(id) => (TrashAction::Restore, body.ids.contains(&id)),
        TrashAsk::Empty { ids, bulk } => {
            let mut asked = ids.to_vec();
            asked.sort_unstable();
            asked.dedup();
            (
                TrashAction::Empty,
                !bulk && asked.len() == ids.len() && asked == body.ids,
            )
        }
    };
    first_mismatch([
        (MismatchField::Cluster, body.cluster == cluster),
        (MismatchField::ThisMachine, body.machine == facts.machine),
        (MismatchField::TrashAction, body.action == action),
        (MismatchField::TrashEntries, entries),
    ])?;
    validity::verify(body.issued_at, body.expires_at, facts.now)?;
    Ok(by)
}

#[cfg(test)]
#[path = "trash_tests.rs"]
mod tests;
