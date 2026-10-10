//! Proposals: what a draft included from another Environment, until it is saved or
//! deployed. Including a source (a Sync now) records one proposal per source in the
//! destination and makes it the owner of the arrivals it landed (`proposal_id` on
//! `config_sync_arrival`). A row has at most one owner, and the first write to the
//! draft that changes an owned row releases it, so Remove puts back exactly what the
//! proposal still owns. Every ownership rule lives here.

use super::pair::{self, Arrival, Which};
use super::*;
use crate::id::{ProposalId, RepositoryId};
use crate::pull_request::PullRequestRef;
use crate::storage::Param;

/// Remove a proposal from a draft: what it still owns there goes back to what the
/// draft held before it was included, and the proposal is forgotten. Rows the draft
/// changed since stay as they are.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveProposal {
    /// The draft's Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// As [`Included::proposal`].
    pub proposal: ProposalId,
    /// Refuse with `conflict` unless this is still the latest `diff` version.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
}

/// A proposal removed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Removed {
    /// The Environment now.
    pub environment: EnvironmentSummary,
    /// False when the draft held no such proposal, as when a Remove is retried.
    pub removed: bool,
}

/// A source included in a draft, as its review lists it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Included {
    /// Pass to [`RemoveProposal::proposal`] to remove it.
    pub proposal: ProposalId,
    /// Where it came from.
    pub source: ProposalSource,
    /// The source's Working State revision it was last included at.
    pub revision: Revision,
    /// The source changed since: review the Sync again to include its changes.
    pub newer: bool,
    /// How many rows of the draft it still owns.
    pub changes: usize,
    /// The Sync that last included it.
    pub sync: SyncId,
}

/// Where an included proposal came from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalSource {
    /// An Environment of the Project.
    Environment {
        /// Its ID.
        id: EnvironmentId,
        /// Its name, or the one it had when it was last included.
        name: String,
        /// False once it is gone.
        live: bool,
    },
    /// A pull request's preview.
    PullRequest {
        /// The repository.
        repository_id: RepositoryId,
        /// The pull request.
        number: PullRequestNumber,
        /// Its preview's name, or the one it had when it was last included.
        name: String,
        /// Its preview now; none once it is gone.
        environment: Option<EnvironmentId>,
    },
}

/// Who a source is to the drafts it is included in: a pull request's previews are one
/// source however often they are recreated.
pub(crate) enum Identity {
    Environment(EnvironmentId),
    PullRequest(PullRequestRef),
}

/// One proposal of a draft.
pub(crate) struct Proposal {
    pub(crate) id: ProposalId,
    /// The source Environment it was last included from.
    source: EnvironmentId,
    /// That source's name then.
    name: String,
    first_sync: SyncId,
    pub(crate) last_sync: SyncId,
}

/// What the proposals of a draft own there, as a Sync from one source plans with it.
#[derive(Default)]
pub(crate) struct Owners {
    /// Rows other proposals own, by the name of the source that owns each.
    pub(crate) held: BTreeMap<RowId, String>,
    /// This source's accepted cells at the rows it owns.
    pub(crate) accepted: BTreeMap<RowId, Cell>,
    /// The rows it owns.
    pub(crate) owned: BTreeSet<RowId>,
}

/// Who `from` is to the drafts it is included in.
pub(crate) fn identity(tx: &mut dyn Tx, from: &Environment) -> Result<Identity, RpcError> {
    Ok(match crate::pull_request::of(tx, &from.summary.id)? {
        Some(pr) => Identity::PullRequest(pr),
        None => Identity::Environment(from.summary.id.clone()),
    })
}

const PROPOSAL: &str = "SELECT id, source_environment_id, source_name, first_sync, last_sync \
     FROM config_proposal WHERE environment_id = ?1";

fn proposal(row: &storage::Row) -> Result<Proposal, RpcError> {
    Ok(Proposal {
        id: row.parse(0, "proposal")?,
        source: row.parse(1, "proposal")?,
        name: row.text(2)?.to_owned(),
        first_sync: row.parse(3, "proposal")?,
        last_sync: row.parse(4, "proposal")?,
    })
}

/// The proposal of `who` in the draft of `into`, if it is included there.
pub(crate) fn find(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    who: &Identity,
) -> Result<Option<Proposal>, RpcError> {
    let rows = match who {
        Identity::Environment(source) => tx.query(
            &format!("{PROPOSAL} AND source_environment_id = ?2"),
            &[into.as_str().into(), source.as_str().into()],
        )?,
        Identity::PullRequest(pr) => tx.query(
            &format!("{PROPOSAL} AND repository_id = ?2 AND number = ?3"),
            &[
                into.as_str().into(),
                pr.repository_id.into(),
                pr.number.into(),
            ],
        )?,
    };
    rows.first().map(proposal).transpose()
}

fn by_id(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    id: &ProposalId,
) -> Result<Option<Proposal>, RpcError> {
    tx.query(
        &format!("{PROPOSAL} AND id = ?2"),
        &[into.as_str().into(), id.as_str().into()],
    )?
    .first()
    .map(proposal)
    .transpose()
}

/// What the proposals of `into`'s draft own, for a Sync from the source whose
/// proposal is `this`.
pub(crate) fn owners(
    tx: &mut dyn Tx,
    into: &EnvironmentId,
    this: Option<&ProposalId>,
) -> Result<Owners, RpcError> {
    let rows = tx.query(
        "SELECT a.lineage, a.at, a.proposal_id, a.source, p.source_name \
         FROM config_sync_arrival a JOIN config_proposal p ON p.id = a.proposal_id \
         WHERE a.environment_id = ?1 AND a.proposal_id IS NOT NULL",
        &[into.as_str().into()],
    )?;
    let mut owners = Owners::default();
    for row in &rows {
        let id = row_id(row.text(0)?, row.text(1)?)?;
        if this.is_some_and(|this| this.as_str() == row.text(2).unwrap_or_default()) {
            owners.accepted.insert(id.clone(), row.json(3, "proposal")?);
            owners.owned.insert(id);
        } else {
            owners.held.insert(id, row.text(4)?.to_owned());
        }
    }
    Ok(owners)
}

/// The Sync of `from` into `into`, planned with what `into`'s proposals own, and
/// `from`'s own proposal there if it is included.
pub(crate) fn planned(
    tx: &mut dyn Tx,
    from: &Environment,
    into: &Environment,
) -> Result<(Move, Option<Proposal>, Owners), RpcError> {
    let who = identity(tx, from)?;
    let found = find(tx, &into.summary.id, &who)?;
    let owners = owners(tx, &into.summary.id, found.as_ref().map(|found| &found.id))?;
    let way = Move::include(tx, from, into, &owners)?;
    Ok((way, found, owners))
}

/// Refuse `picks` that take a row another proposal owns.
pub(crate) fn refuse_held(
    rows: &[PlannedRow],
    sides: &[&SavedEnvironmentIntent; 2],
    picks: &BTreeSet<RowId>,
    owners: &Owners,
) -> Result<(), RpcError> {
    for row in rows {
        if !picks.contains(&row.id) || row.verdict != Verdict::Differs(Why::Included) {
            continue;
        }
        let Some(holder) = owners.held.get(&row.id) else {
            continue;
        };
        let label = named(sides, &row.id).map_or_else(|| row.id.to_string(), |n| n.to_string());
        return Err(error::conflict(
            format!("{label} is included with {holder}: Remove {holder} first, or leave it out"),
            json!({ "row": row.id, "held_by": holder }),
        ));
    }
    Ok(())
}

/// A recreated preview of a pull request included in `into` is the same proposal:
/// point it, and the rows it owns, at the new preview. Refused when a row it owns
/// already arrived from the new preview, which would make two of one row.
pub(crate) fn rebind(
    tx: &mut dyn Tx,
    into: &Environment,
    found: &mut Proposal,
    from: &Environment,
) -> Result<(), RpcError> {
    if found.source == from.summary.id {
        return Ok(());
    }
    let id = &into.summary.id;
    let collides = tx.query(
        "SELECT a.lineage, a.at FROM config_sync_arrival a \
         JOIN config_sync_arrival o ON o.environment_id = a.environment_id \
         AND o.lineage = a.lineage AND o.at = a.at \
         WHERE a.environment_id = ?1 AND a.other_id = ?2 AND o.proposal_id = ?3",
        &[
            id.as_str().into(),
            from.summary.id.as_str().into(),
            found.id.as_str().into(),
        ],
    )?;
    if let Some(row) = collides.first() {
        let row = row_id(row.text(0)?, row.text(1)?)?;
        let label =
            named(&[&into.working], &row).map_or_else(|| row.to_string(), |n| n.to_string());
        return Err(error::conflict(
            format!(
                "{label} already arrived from {}: Remove {} first",
                from.summary.name, found.name
            ),
            json!({ "row": row, "proposal": found.id }),
        ));
    }
    let other = tx.query(
        "SELECT source_name FROM config_proposal \
         WHERE environment_id = ?1 AND source_environment_id = ?2",
        &[id.as_str().into(), from.summary.id.as_str().into()],
    )?;
    if let Some(other) = other.first() {
        let other = other.text(0)?;
        return Err(error::conflict(
            format!("{other} is included already: Remove {other} first"),
            json!({ "proposal": found.id }),
        ));
    }
    tx.execute(
        "UPDATE config_proposal SET source_environment_id = ?1, source_name = ?2 WHERE id = ?3",
        &[
            from.summary.id.as_str().into(),
            from.summary.name.as_str().into(),
            found.id.as_str().into(),
        ],
    )?;
    found.source = from.summary.id.clone();
    found.name = from.summary.name.to_string();
    Ok(())
}

/// `from` is included in `into`'s draft by Sync `sync`, at its revision now: the
/// proposal `found` advanced, or a new one. Returns its ID.
pub(crate) fn record(
    tx: &mut dyn Tx,
    who: &Actor,
    into: &EnvironmentId,
    from: &Environment,
    (found, identity): (Option<&Proposal>, &Identity),
    sync: &SyncId,
) -> Result<ProposalId, RpcError> {
    let at = scope::revision_param(from.summary.revision)?;
    if let Some(found) = found {
        tx.execute(
            "UPDATE config_proposal SET source_revision = ?1, last_sync = ?2, source_name = ?3 \
             WHERE id = ?4",
            &[
                at.into(),
                sync.as_str().into(),
                from.summary.name.as_str().into(),
                found.id.as_str().into(),
            ],
        )?;
        return Ok(found.id.clone());
    }
    let id = ProposalId::parse(uuid::Uuid::new_v4().to_string())?;
    let (repository, number): (Param, Param) = match identity {
        Identity::PullRequest(pr) => (pr.repository_id.into(), pr.number.into()),
        Identity::Environment(_) => (Param::NullInt, Param::NullInt),
    };
    tx.execute(
        "INSERT INTO config_proposal (id, organization_id, environment_id, \
         source_environment_id, repository_id, number, source_name, source_revision, \
         first_sync, last_sync) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            into.as_str().into(),
            from.summary.id.as_str().into(),
            repository,
            number,
            from.summary.name.as_str().into(),
            at.into(),
            sync.as_str().into(),
        ],
    )?;
    Ok(id)
}

/// What a retried Include whose Sync `found` already recorded returns: that Sync's
/// nodes, as they stand, and whether `from` is closing. Nothing is planned or
/// written, so a retry asking to close a Branch its Sync left open is refused.
pub(crate) fn receipt(
    tx: &mut dyn Tx,
    (from, into): (Environment, Environment),
    found: &Proposal,
    close_after: bool,
) -> Result<Synced, RpcError> {
    let closing = tx
        .query(
            "SELECT closing FROM config_environment_branch WHERE environment_id = ?1",
            &[from.summary.id.as_str().into()],
        )?
        .first()
        .map(|row| row.int(0))
        .transpose()?
        .is_some_and(|closing| closing != 0);
    if close_after && !closing {
        return Err(error::conflict(
            format!(
                "Sync {} already ran without close_after: sync again to close {}",
                found.last_sync, from.summary.name
            ),
            json!({}),
        ));
    }
    let rows = tx.query(
        "SELECT lineage, at FROM config_sync_arrival \
         WHERE environment_id = ?1 AND proposal_id = ?2 AND sync_id = ?3",
        &[
            into.summary.id.as_str().into(),
            found.id.as_str().into(),
            found.last_sync.as_str().into(),
        ],
    )?;
    let mut staged = BTreeSet::new();
    for row in &rows {
        let row = row_id(row.text(0)?, row.text(1)?)?;
        if let Some(named) = named(&[&into.working], &row) {
            staged.insert(named.node);
        }
    }
    Ok(Synced {
        sync: found.last_sync.clone(),
        from: from.summary,
        into: into.summary,
        when: SyncedWhen::Now {
            staged: staged.into_iter().collect(),
            closing,
            proposal: found.id.clone(),
        },
    })
}

/// Remove proposal `request.proposal` from its draft.
pub(crate) fn remove(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &RemoveProposal,
) -> Result<Removed, RpcError> {
    let id = scope::environment(tx, who, &request.environment)?
        .summary
        .id;
    scope::lock_project(tx, &id)?;
    let mut into = scope::lock_id(tx, who, &id)?;
    // Absent is the answer a retried Remove gets, whatever the version.
    let Some(proposal) = by_id(tx, &id, &request.proposal)? else {
        return Ok(Removed {
            environment: into.summary,
            removed: false,
        });
    };
    review::check(&review::review(tx, &into)?, request.version.as_deref())?;
    take_out(tx, &mut into, &proposal)?;
    Ok(Removed {
        environment: into.summary,
        removed: true,
    })
}

/// Put back in `into`, whose lock the caller holds, what `proposal` still owns there,
/// rewind the pair base, and forget the proposal. Refused when another node uses a
/// node it brought, or a row of one was edited since.
fn take_out(tx: &mut dyn Tx, into: &mut Environment, proposal: &Proposal) -> Result<(), RpcError> {
    let id = into.summary.id.clone();
    let owned = pair::arrived(tx, &id, Which::Owned(&proposal.id))?;
    let rows: BTreeSet<&RowId> = owned.iter().map(|arrived| &arrived.row).collect();
    let source = &proposal.name;
    let label = |row: &RowId| named(&[&into.working], row);
    for node in rows.iter().filter(|row| *row.at() == At::Node) {
        let Some(dependent) = dependents(&into.working, node.lineage())
            .into_iter()
            .find(|dependent| !rows.contains(dependent))
        else {
            continue;
        };
        let node_name = label(node).map_or_else(|| node.to_string(), |n| n.node.to_string());
        let row = label(&dependent).map_or_else(|| dependent.to_string(), |n| n.to_string());
        let holder = tx.query(
            "SELECT p.source_name FROM config_sync_arrival a \
             JOIN config_proposal p ON p.id = a.proposal_id \
             WHERE a.environment_id = ?1 AND a.lineage = ?2 AND a.at = ?3",
            &[
                id.as_str().into(),
                dependent.lineage().into(),
                dependent.at().to_string().as_str().into(),
            ],
        )?;
        let then = match holder.first() {
            Some(holder) => format!("Remove {} first", holder.text(0)?),
            None => format!("change {row} first"),
        };
        return Err(error::conflict(
            format!("{row} uses {node_name} from {source}: {then}"),
            json!({ "row": dependent, "proposal": proposal.id }),
        ));
    }
    let mut others = Vec::new();
    let mut landed = Vec::new();
    for arrived in owned {
        let Arrival::Pending { prior, was, .. } = arrived.arrival else {
            return Err(error::corrupt("proposal"));
        };
        others.push(arrived.other);
        landed.push(Landed {
            row: arrived.row,
            value: arrived.value,
            prior,
            was,
        });
    }
    // A node it brought goes whole, so a row of it the draft made its own (released,
    // still pending) is an edit Remove would lose; so is a node from the source the
    // draft edited, which the draft can't keep without what it was brought with.
    let nodes: BTreeSet<&str> = landed
        .iter()
        .filter(|landed| *landed.row.at() == At::Node)
        .map(|landed| landed.row.lineage())
        .collect();
    let released = pair::arrived(tx, &id, Which::From(&proposal.source))?
        .into_iter()
        .find(|arrived| {
            matches!(arrived.arrival, Arrival::Pending { owner: None, .. })
                && (nodes.contains(arrived.row.lineage()) || *arrived.row.at() == At::Node)
        })
        .map(|arrived| arrived.row);
    let unapplied = match released {
        Some(row) => Err(Unapplied::Changed(row)),
        None => unapply(&into.working, &suffix(tx, into)?, &landed),
    };
    into.working = match unapplied {
        Ok(working) => working,
        Err(Unapplied::Changed(row)) => {
            let (node, at) = label(&row).map_or_else(
                || (row.lineage().to_owned(), None),
                |n| (n.node.to_string(), n.name),
            );
            let edited = at.map_or_else(|| node.clone(), |at| format!("{node}'s {at}"));
            return Err(error::conflict(
                format!(
                    "{edited} was edited here since {source} was included: \
                     Discard {node}, or keep {source}"
                ),
                json!({ "row": row, "proposal": proposal.id }),
            ));
        }
        Err(Unapplied::Invalid(error)) => return Err(config(error)),
    };
    tx.execute(
        "DELETE FROM config_sync_arrival WHERE environment_id = ?1 AND proposal_id = ?2",
        &[id.as_str().into(), proposal.id.as_str().into()],
    )?;
    let priors = others
        .into_iter()
        .zip(landed)
        .map(|(other, landed)| (other, landed.row, landed.prior))
        .collect();
    pair::rewind_bases(tx, &id, priors)?;
    tx.execute(
        "DELETE FROM config_proposal WHERE id = ?1",
        &[proposal.id.as_str().into()],
    )?;
    // Last: the rows put back are no longer owned, so nothing is left to release.
    scope::save_working(tx, into)
}

/// Undo Sync `sync` if it included a proposal: Remove it while that Sync is its only
/// one, refused once it was included again. None when no proposal holds `sync`.
pub(crate) fn undo(
    tx: &mut dyn Tx,
    who: &Actor,
    sync: &SyncId,
) -> Result<Option<Undone>, RpcError> {
    let rows = tx.query(
        "SELECT environment_id, id FROM config_proposal p \
         WHERE organization_id = ?2 AND (first_sync = ?1 OR last_sync = ?1 OR EXISTS ( \
         SELECT 1 FROM config_sync_arrival a WHERE a.proposal_id = p.id AND a.sync_id = ?1))",
        &[sync.as_str().into(), who.organization.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let (receiver, id) = (
        row.parse::<EnvironmentId>(0, "proposal")?,
        row.parse::<ProposalId>(1, "proposal")?,
    );
    scope::lock_project(tx, &receiver)?;
    let mut into = scope::lock_id(tx, who, &receiver)?;
    let proposal = by_id(tx, &receiver, &id)?.ok_or_else(|| error::corrupt("proposal"))?;
    if proposal.first_sync != *sync || proposal.last_sync != *sync {
        return Err(error::conflict(
            format!(
                "{} was refreshed since: Remove it in Changes",
                proposal.name
            ),
            json!({ "proposal": proposal.id }),
        ));
    }
    // Undo is the whole Sync or nothing: a row of it the draft changed since was
    // released to the draft, and Remove would keep it, so Undo is refused as before.
    let changed = pair::arrived(tx, &receiver, Which::Of(sync))?
        .into_iter()
        .find(|arrived| matches!(arrived.arrival, Arrival::Pending { owner: None, .. }));
    if let Some(changed) = changed {
        let label = named(&[&into.working], &changed.row)
            .map_or_else(|| changed.row.to_string(), |row| row.to_string());
        return Err(error::conflict(
            format!("{label} changed since it synced: change it back instead"),
            json!({ "row": changed.row }),
        ));
    }
    take_out(tx, &mut into, &proposal)?;
    Ok(Some(Undone { into: into.summary }))
}

/// The draft of `environment` is saved or deployed, or discarded whole: its
/// proposals end, and the rows they owned are ordinary arrivals. True when it had any.
pub(crate) fn consume(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<bool, RpcError> {
    tx.execute(
        "UPDATE config_sync_arrival SET proposal_id = NULL, source = NULL \
         WHERE environment_id = ?1 AND proposal_id IS NOT NULL",
        &[environment.as_str().into()],
    )?;
    let ended = tx.execute(
        "DELETE FROM config_proposal WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    Ok(ended > 0)
}

/// Release each row `environment`'s Working State now holds other than its proposal
/// landed there: the draft changed it, so it is the draft's own.
pub(crate) fn release_changed(tx: &mut dyn Tx, environment: &Environment) -> Result<(), RpcError> {
    let id = &environment.summary.id;
    let rows = tx.query(
        "SELECT other_id, lineage, at, value FROM config_sync_arrival \
         WHERE environment_id = ?1 AND proposal_id IS NOT NULL",
        &[id.as_str().into()],
    )?;
    if rows.is_empty() {
        return Ok(());
    }
    let cells = Cells::of(&environment.working, &suffix(tx, environment)?);
    for row in &rows {
        let at = row_id(row.text(1)?, row.text(2)?)?;
        if *cells.at(&at) == row.json::<Cell>(3, "Sync")? {
            continue;
        }
        tx.execute(
            "UPDATE config_sync_arrival SET proposal_id = NULL, source = NULL \
             WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4",
            &[
                id.as_str().into(),
                row.text(0)?.into(),
                row.text(1)?.into(),
                row.text(2)?.into(),
            ],
        )?;
    }
    Ok(())
}

/// The proposals included in `environment`'s draft.
pub(crate) fn included(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<Included>, RpcError> {
    let rows = tx.query(
        "SELECT p.id, p.source_environment_id, p.source_name, p.source_revision, p.last_sync, \
         p.repository_id, p.number, \
         (SELECT COUNT(*) FROM config_sync_arrival a \
          WHERE a.environment_id = p.environment_id AND a.proposal_id = p.id), \
         e.name, e.working_revision \
         FROM config_proposal p LEFT JOIN config_environment e ON e.id = p.source_environment_id \
         WHERE p.environment_id = ?1 ORDER BY p.source_name, p.id",
        &[environment.as_str().into()],
    )?;
    rows.iter()
        .map(|row| {
            let revision = Revision(row.number::<u64>(3, "proposal")?);
            let now = row.optional_int(9)?;
            let name = row.optional_text(8)?.unwrap_or(row.text(2)?).to_owned();
            let id: EnvironmentId = row.parse(1, "proposal")?;
            let live = now.is_some();
            let source = match (row.optional_int(5)?, row.optional_int(6)?) {
                (Some(_), Some(_)) => ProposalSource::PullRequest {
                    repository_id: row.number(5, "proposal")?,
                    number: row.number(6, "proposal")?,
                    name,
                    environment: live.then_some(id),
                },
                _ => ProposalSource::Environment { id, name, live },
            };
            Ok(Included {
                proposal: row.parse(0, "proposal")?,
                source,
                revision,
                newer: now.is_some_and(|now| now > row.int(3).unwrap_or(i64::MAX)),
                changes: usize::try_from(row.int(7)?).map_err(|_| error::corrupt("proposal"))?,
                sync: row.parse(4, "proposal")?,
            })
        })
        .collect()
}
