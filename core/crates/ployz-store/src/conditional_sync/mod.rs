//! Offers: a PR Environment's Sync into one of its Destinations at the merge.
//!
//! An offer is a proposal of the Destination's draft that is not in it yet
//! (`config_proposal.offered`): it owns no Working State rows and leaves the
//! Destination's version and History as they were. It stores what the Sync picked
//! and what including it needs (the PR Environment's side as synced, its secrets'
//! values sealed out, and the values given for them sealed), so [`IncludeProposal`]
//! never reads the PR Environment, which may be gone by then. Someone in the
//! Destination includes it once the pull request merged; Save and Deploy refuse a
//! draft that includes a pull request still unmerged.

mod include;
mod syncing;
pub(crate) use include::include;
pub use include::{IncludeProposal, ProposalIncluded};
pub(crate) use syncing::sync;

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{
    Arrives, Cell, Hostnames, Policy as Rules, RowId, SavedEnvironmentIntent, SealedSecret,
    Verdict, Way,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use crate::branch::{self, Carried, Move, NamedRow};
use crate::id::{
    BranchName, ConditionalSyncId, ProposalId, PullRequestNumber, RepositoryId, Revision,
};
use crate::pull_request;
use crate::scope::{self, Environment, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, error};

/// A Sync offered to a Destination, as the Sync answers it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConditionalSync {
    /// The offer: the proposal [`IncludeProposal`] includes.
    pub id: ConditionalSyncId,
    /// The pull request it waits for.
    pub pull_request: PullRequestNumber,
    /// The rows it holds.
    pub rows: Vec<NamedRow>,
    /// Where it is.
    pub state: ConditionalSyncState,
}

/// Where an offer is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ConditionalSyncState {
    /// Offered to the Destination, until someone there includes or removes it.
    Standing,
}

/// What Cloud checks before telling the Store about a push to a branch.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PendingSyncs {
    /// Pull requests into the branch that a draft includes or is offered, not known
    /// to have merged: Cloud reports each that did.
    pub standing: Vec<PullRequestNumber>,
}

/// What an offer stores: everything including it needs, so it never reads the PR
/// Environment, which may be gone by then.
#[derive(Serialize, Deserialize)]
pub(crate) struct Stored {
    pub(crate) picks: Vec<Pick>,
    /// The PR Environment's Working State as synced, its secrets' values sealed out.
    pub(crate) from: SavedEnvironmentIntent,
    /// What it and the Destination last shared then.
    pub(crate) base: SavedEnvironmentIntent,
    pub(crate) hostnames: Hostnames,
    /// What its Services carry: registry credentials and Deployment Policies.
    pub(crate) carried: Carried,
    /// The PR Environment.
    pub(crate) environment: EnvironmentSummary,
}

/// A picked row.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Pick {
    /// The Destination's cell the Sync was reviewed against.
    pub(crate) reviewed: Cell,
    /// The row, as the Sync named it.
    pub(crate) at: NamedRow,
    /// The pull request's value, as the Sync showed it.
    pub(crate) from: Value,
}

/// An offer as `config_proposal.offered` holds it.
pub(crate) struct Offer {
    pub(crate) stored: Stored,
    /// The values given for its secrets, sealed.
    pub(crate) held: BTreeMap<RowId, SealedSecret>,
}

impl Offer {
    /// `{"v":1,"stored":"<Stored>","held":{"<lineage>/<at>":"<SealedSecret>"}}`: what
    /// it stores kept as text, so converting a Conditional Sync copies it as it was.
    pub(crate) fn encode(stored: &Stored, held: &BTreeMap<RowId, SealedSecret>) -> String {
        let held: Map<String, Value> = held
            .iter()
            .map(|(row, sealed)| {
                (
                    format!("{}/{}", row.lineage(), row.at()),
                    Value::String(branch::json_of(sealed)),
                )
            })
            .collect();
        let stored = serde_json::to_string(stored).expect("an offer is JSON");
        json!({ "v": 1, "stored": stored, "held": held }).to_string()
    }

    /// Offer `id` from its text; corrupt, naming it, when it can't be read.
    pub(crate) fn decode(id: &ProposalId, text: &str) -> Result<Self, RpcError> {
        let corrupt = || error::corrupt(&format!("offer {id}"));
        let Ok(Value::Object(offer)) = serde_json::from_str::<Value>(text) else {
            return Err(corrupt());
        };
        if offer.get("v") != Some(&json!(1)) {
            return Err(corrupt());
        }
        let stored = offer
            .get("stored")
            .and_then(Value::as_str)
            .and_then(|stored| serde_json::from_str(stored).ok())
            .ok_or_else(corrupt)?;
        let mut held = BTreeMap::new();
        if let Some(values) = offer.get("held") {
            let Value::Object(values) = values else {
                return Err(corrupt());
            };
            for (key, value) in values {
                let (lineage, at) = key.split_once('/').ok_or_else(corrupt)?;
                let row = branch::row_id(lineage, at).map_err(|_| corrupt())?;
                let sealed = value
                    .as_str()
                    .and_then(|value| serde_json::from_str(value).ok())
                    .ok_or_else(corrupt)?;
                held.insert(row, sealed);
            }
        }
        Ok(Self { stored, held })
    }

    /// Its Sync into `into` now, held off the rows `into`'s proposals own.
    pub(crate) fn planned(
        &self,
        tx: &mut dyn Tx,
        into: &Environment,
        held: impl IntoIterator<Item = RowId>,
    ) -> Result<Move, RpcError> {
        let stored = &self.stored;
        let rules = Rules {
            way: Way::Sync,
            from_marks: BTreeSet::new(),
            into_marks: branch::marked(tx, &into.summary.id)?,
            live: branch::used_live(&into.working).into_keys().collect(),
            own: None,
            held: held.into_iter().collect(),
            accepted: BTreeMap::new(),
        };
        Ok(Move::offered(
            stored.environment.id.clone(),
            &into.summary.name,
            (
                stored.from.clone(),
                stored.base.clone(),
                stored.hostnames.clone(),
            ),
            rules,
            stored.carried.clone(),
        ))
    }
}

/// The pull requests into `branch` of `repository_id` a draft includes or is
/// offered, not known to have merged.
pub(crate) fn pending(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    branch: &BranchName,
) -> Result<PendingSyncs, RpcError> {
    let rows = tx.query(
        "SELECT DISTINCT p.number, r.facts FROM config_proposal p \
         JOIN config_pull_request r ON r.organization_id = p.organization_id \
         AND r.repository_id = p.repository_id AND r.number = p.number \
         WHERE p.organization_id = ?1 AND p.repository_id = ?2 AND r.merged IS NULL \
         ORDER BY p.number",
        &[who.organization.as_str().into(), repository_id.into()],
    )?;
    let mut pending = PendingSyncs::default();
    for row in rows {
        let facts: pull_request::PullRequest = row.json(1, "pull request")?;
        let number = row.number(0, "proposal")?;
        if facts.target_branch == *branch && !pending.standing.contains(&number) {
            pending.standing.push(number);
        }
    }
    Ok(pending)
}

/// PR Environment `pr`'s offer or included proposal in Destination `into`, as the
/// pull request's page shows it.
/// A Follow staged the Parent's changes in PR Environment `branch`, whose Working
/// State was at revision `before`: what it offered or had included still stands, as
/// only the author's own edits make a pull request's proposals stale.
pub(crate) fn followed(
    tx: &mut dyn Tx,
    branch: &EnvironmentSummary,
    before: Revision,
) -> Result<(), RpcError> {
    if branch.revision == before {
        return Ok(());
    }
    tx.execute(
        "UPDATE config_proposal SET source_revision = ?3 \
         WHERE source_environment_id = ?1 AND number IS NOT NULL AND source_revision = ?2",
        &[
            branch.id.as_str().into(),
            scope::revision_param(before)?.into(),
            scope::revision_param(branch.revision)?.into(),
        ],
    )?;
    Ok(())
}

pub(crate) fn standing_in(
    tx: &mut dyn Tx,
    pr: &Environment,
    into: &Environment,
) -> Result<Option<pull_request::DestinationSync>, RpcError> {
    let found = match pull_request::of(tx, &pr.summary.id)? {
        Some(reference) => tx.query(
            "SELECT id, source_revision, offered FROM config_proposal \
             WHERE environment_id = ?1 AND repository_id = ?2 AND number = ?3",
            &[
                into.summary.id.as_str().into(),
                reference.repository_id.into(),
                reference.number.into(),
            ],
        )?,
        None => Vec::new(),
    };
    let found = match found.into_iter().next() {
        Some(row) => Some(row),
        None => tx
            .query(
                "SELECT id, source_revision, offered FROM config_proposal \
                 WHERE environment_id = ?1 AND source_environment_id = ?2",
                &[
                    into.summary.id.as_str().into(),
                    pr.summary.id.as_str().into(),
                ],
            )?
            .into_iter()
            .next(),
    };
    let Some(row) = found else {
        return Ok(None);
    };
    let proposal: ProposalId = row.parse(0, "proposal")?;
    let standing = row.number::<u64>(1, "proposal")? == pr.summary.revision.0;
    let (changes, waiting) = match row.optional_text(2)? {
        Some(text) => {
            let offer = Offer::decode(&proposal, text)?;
            let owned = branch::proposal_owners(tx, &into.summary.id, None)?;
            let plan = offer
                .planned(tx, into, owned.held.into_keys())?
                .plan(&into.working);
            let waiting = plan
                .rows()
                .iter()
                .filter(|row| {
                    matches!(
                        row.verdict,
                        Verdict::Moves {
                            arrives: Arrives::NeedsValue,
                            ..
                        }
                    ) && !offer.held.contains_key(&row.id)
                })
                .filter_map(|row| offer.stored.picks.iter().find(|pick| pick.at.row == row.id))
                .map(|pick| pick.at.to_string())
                .collect();
            (offer.stored.picks.len(), waiting)
        }
        None => {
            let owned = tx.query(
                "SELECT COUNT(*) FROM config_sync_arrival \
                 WHERE environment_id = ?1 AND proposal_id = ?2",
                &[into.summary.id.as_str().into(), proposal.as_str().into()],
            )?;
            let owned = owned
                .first()
                .ok_or_else(|| error::corrupt("proposal"))?
                .number::<usize>(0, "proposal")?;
            (owned, Vec::new())
        }
    };
    Ok(Some(pull_request::DestinationSync {
        id: ConditionalSyncId::parse(proposal.as_str())?,
        standing,
        changes,
        waiting,
    }))
}
