//! Including an offer in its Destination's draft.

use crate::settings::NodeName;

use super::*;
use crate::SealingKey;
use crate::branch::{Guard, Owner, RowRef};
use crate::id::SyncId;
use crate::scope::{self, EnvironmentRef};

/// Include an offer in its Destination's draft: what it picked lands there as a Sync
/// now would, owned by the offer, which is then an included proposal. A row the
/// Destination changed since the Sync was reviewed stays as it is, and is answered
/// in [`ProposalIncluded::kept`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct IncludeProposal {
    /// The draft's Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The offer, as the draft's review lists it.
    pub proposal: ProposalId,
    /// Refused with `conflict` unless this is still the latest `diff` version.
    pub version: String,
    /// A value for each secret the draft lacks that the Sync gave none for: sealed at
    /// once, never shown back.
    #[serde(default)]
    #[ts(as = "Option<BTreeMap<RowRef, String>>", optional)]
    pub values: BTreeMap<RowRef, String>,
}

/// An offer included.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ProposalIncluded {
    /// The Environment now.
    pub environment: EnvironmentSummary,
    /// The proposal.
    pub proposal: ProposalId,
    /// The Sync that offered it: pass it to [`crate::UndoSync::sync`] to undo it.
    pub sync: SyncId,
    /// Nodes staged in the draft.
    pub staged: Vec<NodeName>,
    /// Picked rows the draft changed since the Sync was reviewed: they stay as they are.
    pub kept: Vec<NamedRow>,
    /// False when the draft held no such offer, as when an Include is retried.
    pub included: bool,
}

pub(crate) fn include(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &IncludeProposal,
) -> Result<ProposalIncluded, RpcError> {
    let id = scope::environment(tx, who, &request.environment)?
        .summary
        .id;
    scope::lock_project(tx, &id)?;
    let mut into = scope::lock_id(tx, who, &id)?;
    let proposal = &request.proposal;
    let rows = tx.query(
        "SELECT first_sync, offered FROM config_proposal WHERE environment_id = ?1 AND id = ?2",
        &[id.as_str().into(), proposal.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Err(error::not_found(
            format!("No proposal {proposal} in {}", into.summary.name),
            json!({ "proposal": proposal }),
        ));
    };
    let sync: SyncId = row.parse(0, "proposal")?;
    let Some(text) = row.optional_text(1)? else {
        // Included already: a retry gets the answer it would have.
        return Ok(ProposalIncluded {
            environment: into.summary,
            proposal: proposal.clone(),
            sync,
            staged: Vec::new(),
            kept: Vec::new(),
            included: false,
        });
    };
    let offer = Offer::decode(proposal, text)?;
    let held = branch::proposal_owners(tx, &id, None)?.held;
    let way = offer.planned(tx, &into, held.into_keys())?;
    let checked = way.check(tx, &into, Guard::Diff(&request.version))?;
    let stored = &offer.stored;
    let mut picked = BTreeSet::new();
    let mut kept = Vec::new();
    for pick in &stored.picks {
        let moves = checked.rows().iter().any(|row| {
            row.id == pick.at.row
                && matches!(row.verdict, Verdict::Moves { .. })
                && row.into == pick.reviewed
        });
        if moves {
            picked.insert(pick.at.row.clone());
        } else {
            kept.push(pick.at.clone());
        }
    }
    let picked = branch::whole(checked.rows(), picked);
    for pick in &stored.picks {
        if !picked.contains(&pick.at.row) && !kept.contains(&pick.at) {
            kept.push(pick.at.clone());
        }
    }
    if picked.is_empty() {
        return Err(error::conflict(
            format!(
                "Nothing of {} still applies to {}: Remove it",
                stored.environment.name, into.summary.name
            ),
            json!({ "proposal": proposal, "kept": kept }),
        ));
    }
    let needs: BTreeSet<&RowId> = checked
        .rows()
        .iter()
        .filter(|row| {
            picked.contains(&row.id)
                && matches!(
                    row.verdict,
                    Verdict::Moves {
                        arrives: Arrives::NeedsValue,
                        ..
                    }
                )
        })
        .map(|row| &row.id)
        .collect();
    let named: Vec<NamedRow> = stored.picks.iter().map(|pick| pick.at.clone()).collect();
    let asked = request
        .values
        .iter()
        .map(|(asked, value)| Ok((branch::resolve_one(asked, &named)?, value.clone())))
        .collect::<Result<BTreeMap<_, _>, RpcError>>()?;
    let sides = [&stored.from, &into.working];
    let given = branch::sealed(sealing, checked.rows(), &sides, &picked, &asked)?;
    let mut values: BTreeMap<RowId, SealedSecret> = offer
        .held
        .iter()
        .filter(|(row, _)| needs.contains(row))
        .map(|(row, sealed)| (row.clone(), sealed.clone()))
        .collect();
    for (row, sealed) in given {
        values.entry(row).or_insert(sealed);
    }
    let missing: Vec<String> = needs
        .iter()
        .filter(|row| !values.contains_key(**row))
        .map(|row| {
            branch::named(&sides, row).map_or_else(|| row.to_string(), |named| named.to_string())
        })
        .collect();
    if !missing.is_empty() {
        return Err(error::conflict(
            format!("{} needs a value", missing.join(", ")),
            json!({ "needs_value": missing }),
        ));
    }
    tx.execute(
        "UPDATE config_proposal SET offered = NULL WHERE id = ?1",
        &[proposal.as_str().into()],
    )?;
    let before = branch::credentials(tx, &into)?;
    let owner = Owner::Proposal {
        id: proposal,
        sync: &sync,
    };
    let staged = checked.apply(tx, who, &mut into, &picked, &values, owner)?;
    branch::keep_carried(tx, &into, proposal, &before)?;
    Ok(ProposalIncluded {
        environment: into.summary,
        proposal: proposal.clone(),
        sync,
        staged,
        kept,
        included: true,
    })
}
