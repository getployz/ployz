//! A PR Environment's On merge list: what its pull request's merge lands in each
//! Destination. The list is derived each time, a Sync over the Destination's Saved
//! State whose removals move too and whose rows land even where the Destination
//! changed them. Exclude is a Never sync mark on the PR Environment; only an Edit, the
//! value a row lands with in place of the PR Environment's, is stored.

use std::collections::BTreeMap;

use ployz_core::config::{
    Arrives, At, Cell, Cells, PRIVATE_DOMAIN_KEY, Plan, RowId, SERVICE_NAME_KEY,
    SavedEnvironmentIntent, SavedVariableValue, SealedCell, ServiceGitBranch, ServiceSource,
    Setting, ValuePart, ValuePartOwner, Verdict, Why, canonicalize_environment_intent, marks_on,
};
use serde_json::Value;

use super::*;
use crate::SealingKey;
use crate::automation;
use crate::branch::{
    Carried, Move, NamedRow, RowRef, config, end_included, json_of, named, resolve_one, row_id,
    share, shown,
};
use crate::storage;
use crate::variables::{self, VariableKey};

/// Read a PR Environment's On merge list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct OnMergeQuery {
    /// The PR Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// What a PR Environment's pull request lands when it merges.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OnMerge {
    /// The PR Environment.
    pub environment: EnvironmentSummary,
    /// Its pull request.
    pub pull_request: PullRequestRef,
    /// One list per Destination: each Environment that deploys the target branch.
    pub destinations: Vec<OnMergeInto>,
}

/// What the merge lands in one Destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OnMergeInto {
    /// The Destination.
    pub name: EnvironmentName,
    /// Each row the PR Environment changed, riding or not.
    pub rows: Vec<MergeRow>,
}

/// One row of the On merge list. Values read as the Sync view's do: text, `{"secret":
/// true}`, `{"secret": false}` for a secret without a value, or null where the row has
/// none.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MergeRow {
    /// The row.
    #[serde(flatten)]
    #[ts(flatten)]
    pub at: NamedRow,
    /// The Destination's Saved value now.
    pub production: Value,
    /// What lands: the Edit, else the PR Environment's value. Null for a removal; a
    /// secret the Destination lacks lands Not set unless edited.
    pub lands: Value,
    /// The PR Environment's own value.
    pub preview: Value,
    /// Whether the merge lands it.
    pub ride: Ride,
    /// The Destination changed it too since the PR Environment started: the merge
    /// lands over that.
    pub conflict: bool,
    /// The row of the new node it is in, which rides with it.
    pub requires: Option<RowId>,
    /// The rows that still reference the node it removes: it stays until they change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<NamedRow>>", optional)]
    pub refused_by: Vec<NamedRow>,
}

/// Whether a row lands at the merge.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Ride {
    /// It lands with the PR Environment's value.
    Rides,
    /// It lands with the value an Edit gave it.
    Edited,
    /// The PR Environment marks it Never sync: it stays here.
    Excluded,
    /// The Destination marks it Never sync: it keeps its own value.
    ProductionKeeps,
    /// A Volume the PR Environment removed: the Destination keeps it and its data.
    Kept,
}

impl Ride {
    pub(crate) const fn rides(self) -> bool {
        matches!(self, Self::Rides | Self::Edited)
    }
}

/// Give a row of the On merge list the value it lands with in one Destination, in
/// place of the PR Environment's, or take that Edit back. Only a variable that rides
/// takes one; a secret's is sealed at once and never shown back.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetMergeValue {
    /// The PR Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Destination; omitted, its only one.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub into: Option<EnvironmentRef>,
    /// The row, by RowId or by name in either side.
    pub row: RowRef,
    /// The value it lands with; omitted, the Edit is taken back.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub value: Option<String>,
}

/// What a merge lands in one Destination, planned.
pub(crate) struct Landing {
    /// The Destination.
    pub(crate) into: Environment,
    pub(crate) plan: Plan,
    pub(crate) rows: Vec<MergeRow>,
}

impl Landing {
    /// The rows that land: a removal something still references doesn't.
    pub(crate) fn riding(&self) -> BTreeSet<RowId> {
        self.rows
            .iter()
            .filter(|row| row.ride.rides() && row.refused_by.is_empty())
            .map(|row| row.at.row.clone())
            .collect()
    }
}

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &OnMergeQuery,
) -> Result<OnMerge, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    listed(tx, who, environment)
}

/// The On merge list of PR Environment `environment`.
fn listed(tx: &mut dyn Tx, who: &Actor, environment: Environment) -> Result<OnMerge, RpcError> {
    let (pull_request, landings) = landings(tx, who, &environment)?;
    Ok(OnMerge {
        destinations: landings
            .into_iter()
            .map(|landing| OnMergeInto {
                name: landing.into.summary.name,
                rows: landing.rows,
            })
            .collect(),
        pull_request,
        environment: environment.summary,
    })
}

/// What `from`'s pull request lands in each of its Destinations.
pub(crate) fn landings(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &Environment,
) -> Result<(PullRequestRef, Vec<Landing>), RpcError> {
    let Some(pull_request) = of(tx, &from.summary.id)? else {
        return Err(error::invalid(
            format!("{} isn't a PR Environment", from.summary.name),
            json!({}),
        ));
    };
    let target = facts(tx, who, pull_request.repository_id, pull_request.number)?
        .map(|facts| facts.target_branch);
    let mut landings = Vec::new();
    if let Some(target) = target {
        let project = scope::project_of(tx, &from.summary.id)?.id;
        for into in destinations_of(tx, &project, pull_request.repository_id, &target)? {
            let into = scope::load_by_id(tx, &into)?;
            landings.push(landing(tx, from, into)?);
        }
    }
    Ok((pull_request, landings))
}

fn landing(tx: &mut dyn Tx, from: &Environment, into: Environment) -> Result<Landing, RpcError> {
    let saved = review::latest_saved(tx, &into.summary.id)?
        .map_or_else(|| into.working.clone(), |saved| saved.intent);
    let way = Move::merge(tx, from, &into)?;
    let plan = way.plan(&saved);
    let stored = edits(tx, &from.summary.id, &into.summary.id)?;
    let (from_names, into_names) = (from.names(), into.names());
    let mut rows = Vec::new();
    for row in plan.rows() {
        let (ride, conflict, arrives) = match &row.verdict {
            Verdict::Moves {
                ticked: true,
                conflict,
                arrives,
            } => {
                // A removal lands as one: an Edit never brings the row back.
                // An Edit sealed for the other secrecy is dropped, so a secret
                // never lands as plain text nor a plain value as a secret.
                let secret = row.from.is_secret() || row.into.is_secret();
                let edited = stored
                    .get(&row.id)
                    .is_some_and(|edit| matches!(edit, SealedCell::Secret(_)) == secret)
                    && row.from != Cell::Absent;
                let ride = if edited { Ride::Edited } else { Ride::Rides };
                (ride, *conflict, *arrives)
            }
            Verdict::Differs(Why::NeverSynced) => {
                let excluded = marks_on(&row.id, &way.rules.from_marks).next().is_some();
                let ride = if excluded {
                    Ride::Excluded
                } else {
                    Ride::ProductionKeeps
                };
                (ride, false, Arrives::AsIs)
            }
            Verdict::Kept => (Ride::Kept, false, Arrives::AsIs),
            Verdict::Moves { ticked: false, .. } | Verdict::Differs(_) => continue,
        };
        let at = named(&[&from.working, &saved], &row.id)
            .ok_or_else(|| error::corrupt("On merge row"))?;
        let preview = shown(&from.working, &from_names, &row.id, &row.from);
        let production = shown(&saved, &into_names, &row.id, &row.into);
        let lands = match (stored.get(&row.id), arrives) {
            _ if ride == Ride::Kept => production.clone(),
            (Some(edit), _) if ride == Ride::Edited => shown_edit(edit, &into_names),
            (_, Arrives::NeedsValue) => json!({ "secret": false }),
            _ => preview.clone(),
        };
        rows.push(MergeRow {
            production,
            lands,
            preview,
            ride,
            conflict,
            requires: row.requires.clone(),
            refused_by: Vec::new(),
            at,
        });
    }
    // A row of a new node that stays here stays with it.
    let staying: BTreeSet<RowId> = rows
        .iter()
        .filter(|row| !row.ride.rides())
        .map(|row| row.at.row.clone())
        .collect();
    for row in &mut rows {
        if row.ride.rides()
            && row
                .requires
                .as_ref()
                .is_some_and(|node| staying.contains(node))
        {
            row.ride = Ride::Excluded;
        }
    }
    let mut picks: BTreeSet<RowId> = rows
        .iter()
        .filter(|row| row.ride.rides())
        .map(|row| row.at.row.clone())
        .collect();
    let values = edited_values(stored, &rows);
    // Dropping a refused removal keeps its node, whose own references can refuse
    // another removal, so repeat until none is refused.
    let mut refused = BTreeMap::new();
    loop {
        let found = plan.removal_dependents(&picks, &values).map_err(config)?;
        if found.is_empty() {
            break;
        }
        for (removal, dependents) in found {
            picks.remove(&removal);
            refused.insert(removal, dependents);
        }
    }
    for row in &mut rows {
        if let Some(dependents) = refused.remove(&row.at.row) {
            row.refused_by = dependents
                .iter()
                .map(|dependent| {
                    named(&[&from.working, &saved], dependent)
                        .ok_or_else(|| error::corrupt("On merge dependent"))
                })
                .collect::<Result<_, _>>()?;
        }
    }
    Ok(Landing { into, plan, rows })
}

/// The Edits of the rows that land with one.
fn edited_values(
    mut stored: BTreeMap<RowId, SealedCell>,
    rows: &[MergeRow],
) -> BTreeMap<RowId, SealedCell> {
    stored.retain(|id, _| {
        rows.iter()
            .any(|row| row.at.row == *id && row.ride == Ride::Edited)
    });
    stored
}

/// An Edit as reads show it.
fn shown_edit(edit: &SealedCell, names: &BTreeMap<String, String>) -> Value {
    match edit {
        SealedCell::Secret(_) => json!({ "secret": true }),
        SealedCell::Cell(Cell::Value(value)) => {
            let mut value = value.clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("fingerprint");
            }
            serde_json::from_value::<SavedVariableValue>(value)
                .map_or(Value::Null, |value| variables::shown_value(&value, names))
        }
        SealedCell::Cell(_) => Value::Null,
    }
}

/// The Edits stored for the merge of `from` into `into`.
fn edits(
    tx: &mut dyn Tx,
    from: &EnvironmentId,
    into: &EnvironmentId,
) -> Result<BTreeMap<RowId, SealedCell>, RpcError> {
    tx.query(
        "SELECT lineage, at, cell FROM config_merge_value \
         WHERE environment_id = ?1 AND destination_id = ?2",
        &[from.as_str().into(), into.as_str().into()],
    )?
    .iter()
    .map(|row| Ok((row_id(row.text(0)?, row.text(1)?)?, row.json(2, "Edit")?)))
    .collect()
}

pub(crate) fn set_merge_value(
    tx: &mut dyn Tx,
    who: &Actor,
    sealing: &SealingKey,
    request: &SetMergeValue,
) -> Result<OnMerge, RpcError> {
    let from = scope::lock(tx, who, &request.environment)?;
    let (_, landings) = landings(tx, who, &from)?;
    let landing = match &request.into {
        Some(into) => {
            let into = scope::environment(tx, who, into)?.summary;
            landings
                .into_iter()
                .find(|landing| landing.into.summary.id == into.id)
                .ok_or_else(|| {
                    error::invalid(
                        format!(
                            "{}'s merge doesn't land in {}",
                            from.summary.name, into.name
                        ),
                        json!({}),
                    )
                })?
        }
        None => {
            let mut landings = landings.into_iter();
            match (landings.next(), landings.next()) {
                (Some(landing), None) => landing,
                (None, _) => {
                    return Err(error::invalid(
                        format!("{}'s merge lands nowhere", from.summary.name),
                        json!({}),
                    ));
                }
                (Some(_), Some(_)) => {
                    return Err(error::invalid(
                        format!(
                            "{}'s merge lands in more than one Environment: name one",
                            from.summary.name
                        ),
                        json!({}),
                    ));
                }
            }
        }
    };
    let named: Vec<NamedRow> = landing.rows.iter().map(|row| row.at.clone()).collect();
    let row = resolve_one(&request.row, &named)?;
    let label = named
        .iter()
        .find(|named| named.row == row)
        .map_or_else(|| row.to_string(), ToString::to_string);
    let at = row.at().to_string();
    let key_values = [
        from.summary.id.as_str().into(),
        landing.into.summary.id.as_str().into(),
        row.lineage().into(),
        at.as_str().into(),
    ];
    // Taking an Edit back always works, wherever the row went since.
    tx.execute(
        "DELETE FROM config_merge_value \
         WHERE environment_id = ?1 AND destination_id = ?2 AND lineage = ?3 AND at = ?4",
        &key_values,
    )?;
    if let Some(value) = &request.value {
        if !landing.riding().contains(&row) {
            return Err(error::invalid(
                format!("{label} doesn't land in {}", landing.into.summary.name),
                json!({ "row": row }),
            ));
        }
        let At::Variable(key) = row.at() else {
            return Err(error::invalid(
                format!("{label}: only a variable takes an Edit"),
                json!({ "row": row }),
            ));
        };
        let planned = landing
            .plan
            .rows()
            .iter()
            .find(|planned| planned.id == row)
            .ok_or_else(|| error::corrupt("On merge row"))?;
        if planned.from == Cell::Absent {
            return Err(error::invalid(
                format!("{label}: the merge removes it, so it takes no Edit"),
                json!({ "row": row }),
            ));
        }
        let cell = if planned.from.is_secret() || planned.into.is_secret() {
            SealedCell::Secret(crate::branch::seal_secret(sealing, &row, &label, value)?)
        } else {
            let (parts, fingerprint) =
                variables::text_parts(&VariableKey::parse(key)?, value, &landing.into.names())?;
            let mut value = json!(variables::stored(parts));
            if let Some(object) = value.as_object_mut() {
                object.insert("fingerprint".into(), json!(fingerprint));
            }
            SealedCell::Cell(Cell::Value(value))
        };
        let [environment, destination, lineage, at] = key_values;
        tx.execute(
            "INSERT INTO config_merge_value \
             (environment_id, destination_id, organization_id, lineage, at, cell) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            &[
                environment,
                destination,
                who.organization.as_str().into(),
                lineage,
                at,
                json_of(&cell).as_str().into(),
            ],
        )?;
    }
    listed(tx, who, from)
}

/// Whether closing PR Environment `id` would lose something: its pull request is
/// open, or merged and some Destination didn't take it, and it holds a row for
/// some Destination, riding or not. Once the merge landed everywhere, nothing
/// else of it lands. Merged into a branch no Environment tracks, it landed
/// nowhere: it holds what it would sync into its Parent.
pub(crate) fn holds(tx: &mut dyn Tx, who: &Actor, id: &EnvironmentId) -> Result<bool, RpcError> {
    let Some(pull_request) = of(tx, id)? else {
        return Ok(false);
    };
    let open = facts(tx, who, pull_request.repository_id, pull_request.number)?
        .is_some_and(|facts| facts.open);
    let unlanded = unlanded(tx, who, &pull_request)?;
    let from = scope::load_by_id(tx, id)?;
    let (_, mut landings) = landings(tx, who, &from)?;
    if landings.is_empty() && unlanded.is_some() {
        let Some(branch) = crate::branch::row(tx, id)? else {
            return Ok(false);
        };
        let parent = scope::load_by_id(tx, &branch.parent)?;
        landings.push(landing(tx, &from, parent)?);
    } else if !open
        && !unlanded.is_some_and(|unlanded| unlanded.iter().any(|destination| !destination.saved))
    {
        return Ok(false);
    }
    // A Kept Volume stays where it is: nothing of it lands.
    Ok(landings
        .iter()
        .any(|landing| landing.rows.iter().any(|row| row.ride != Ride::Kept)))
}

/// What a Destination didn't take when the pull request merged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Unlanded {
    /// The Destination's Project.
    pub(crate) project: ProjectName,
    /// The Destination.
    pub(crate) into: EnvironmentName,
    /// Whether it saved what landed, but didn't deploy it.
    pub(crate) saved: bool,
    /// Why not.
    pub(crate) reason: String,
}

/// What the pull request's merge didn't do, once it landed; none before.
pub(crate) fn unlanded(
    tx: &mut dyn Tx,
    who: &Actor,
    pull_request: &PullRequestRef,
) -> Result<Option<Vec<Unlanded>>, RpcError> {
    tx.query(
        "SELECT unlanded FROM config_pull_request \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3 AND unlanded IS NOT NULL",
        &[
            who.organization.as_str().into(),
            pull_request.repository_id.into(),
            pull_request.number.into(),
        ],
    )?
    .first()
    .map(|row| row.json(0, "unlanded"))
    .transpose()
}

/// Record what the pull request's merge didn't do.
pub(crate) fn record_unlanded(
    tx: &mut dyn Tx,
    who: &Actor,
    pull_request: &PullRequestRef,
    unlanded: &[Unlanded],
) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_pull_request SET unlanded = ?4 \
         WHERE organization_id = ?1 AND repository_id = ?2 AND number = ?3",
        &[
            who.organization.as_str().into(),
            pull_request.repository_id.into(),
            pull_request.number.into(),
            serde_json::to_string(unlanded)
                .expect("unlanded is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(())
}

/// A pull request's merge, as the Store first learned of it.
pub(crate) struct Merging<'a> {
    /// The merge commit.
    pub(crate) commit: &'a CommitSha,
    /// The branch it merged into.
    pub(crate) target: &'a BranchName,
    /// A head of `target` that GitHub says contains `commit`.
    pub(crate) target_head: Option<&'a CommitSha>,
    pub(crate) trusted: &'a Trusted,
}

/// What a pull request's merge did across its Destinations.
pub(crate) struct Merged {
    /// Whether every Destination took it; false when there is none.
    pub(crate) all: bool,
    /// The IDs of the Services the merge removed, by Destination.
    // TODO: the merge's Deploy tears these down once targeted Deploys can retire a
    // Service; until then they stay running until a full Deploy.
    #[expect(dead_code, reason = "the merge's Deploy doesn't retire Services yet")]
    pub(crate) removals: BTreeMap<EnvironmentId, Vec<String>>,
}

/// What a merge changed in one Destination's Services.
struct MergedServices {
    /// The IDs of the Services it changed that Saved State still has.
    updates: Vec<String>,
    /// The IDs of the Services it removed, as Saved State had them before.
    removals: Vec<String>,
}

/// Land what PR Environment `from`'s pull request lands in each Destination, one
/// Saved revision each, and deploy the Services it changed there. A Destination
/// that can't take it is skipped and keeps what it had. A merge into a branch
/// nothing deploys lands nowhere, so the PR Environment keeps what it holds. What
/// didn't land, or landed but didn't deploy, goes in `unlanded`. The caller holds
/// the locks.
pub(crate) fn land(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &EnvironmentId,
    merge: &Merging<'_>,
    (automated, unlanded): (&mut Automated, &mut Vec<Unlanded>),
) -> Result<Merged, RpcError> {
    let from = scope::load_by_id(tx, from)?;
    let (pull_request, landings) = landings(tx, who, &from)?;
    let mut all = !landings.is_empty();
    let mut removals = BTreeMap::new();
    for landing in landings {
        let into = landing.into.summary.clone();
        let landed = storage::attempt(tx, |tx| {
            land_one(tx, who, &from, landing, (&pull_request, merge.target))
        });
        match landed {
            Ok(MergedServices {
                updates,
                removals: removed,
            }) => {
                if !removed.is_empty() {
                    removals.insert(into.id.clone(), removed);
                }
                if updates.is_empty() {
                    continue;
                }
                let services = updates;
                let skipped = automated.skipped.len();
                automation::deploy_landed(
                    tx,
                    who,
                    &into.id,
                    pull_request.repository_id,
                    merge,
                    services,
                    automated,
                )?;
                unlanded.extend(
                    automated
                        .skipped
                        .iter()
                        .skip(skipped)
                        .filter(|skip| skip.environment == into.id)
                        .map(|skip| Unlanded {
                            project: into.project.clone(),
                            into: into.name.clone(),
                            saved: true,
                            reason: skip.reason.clone(),
                        }),
                );
            }
            Err(error) if automation::skippable(&error) => {
                all = false;
                unlanded.push(Unlanded {
                    project: into.project.clone(),
                    into: into.name.clone(),
                    saved: false,
                    reason: error.message.clone(),
                });
                automated.skipped.push(Skipped {
                    environment: into.id,
                    reason: format!(
                        "Didn't land #{} in {}: {}",
                        pull_request.number, into.name, error.message
                    ),
                });
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Merged { all, removals })
}

/// Land `landing` in its Destination: Saved State takes every riding row, and
/// Working State each one it still holds as Saved did, so an edit staged there
/// stays and wins at the next Save. A Git Service it introduces from the merged
/// repository lands tracking the branch it merged into.
fn land_one(
    tx: &mut dyn Tx,
    who: &Actor,
    from: &Environment,
    landing: Landing,
    (pull_request, target): (&PullRequestRef, &BranchName),
) -> Result<MergedServices, RpcError> {
    let picks = landing.riding();
    let Landing {
        into: mut destination,
        plan,
        rows,
    } = landing;
    if picks.is_empty() {
        return Ok(MergedServices {
            updates: Vec::new(),
            removals: Vec::new(),
        });
    }
    let into = destination.summary.id.clone();
    let values = edited_values(edits(tx, &from.summary.id, &into)?, &rows);
    let mut applied = plan.apply(&picks, &values).map_err(config)?;
    let (removed, arriving): (BTreeSet<&str>, BTreeSet<&str>) = picks
        .iter()
        .filter(|pick| *pick.at() == At::Node)
        .map(RowId::lineage)
        .partition(|lineage| {
            plan.rows()
                .iter()
                .any(|row| row.id == RowId::node(lineage) && row.from == Cell::Absent)
        });
    for service in &mut applied.next.services {
        if let ServiceSource::Git {
            repository_id,
            branch,
            ..
        } = &mut service.config.source
            && *repository_id == pull_request.repository_id.get()
            && arriving.contains(service.lineage_id.as_str())
        {
            *branch = ServiceGitBranch::Connected {
                name: target.to_string(),
            };
        }
    }
    let latest = review::latest_saved(tx, &into)?;
    let old = latest
        .as_ref()
        .map_or_else(|| destination.working.clone(), |saved| saved.intent.clone());
    let removals = old
        .services
        .iter()
        .filter(|service| removed.contains(service.lineage_id.as_str()))
        .map(|service| service.id.clone())
        .collect();
    let (saved, _) = scope::checked(
        tx,
        &destination.summary,
        &old,
        Some(&from.working),
        &applied.next,
    )?;
    review::publish(
        tx,
        who,
        &into,
        saved.clone(),
        latest.as_ref(),
        Some(&format!("Merged #{}", pull_request.number)),
    )?;

    // Working: the nodes arriving as Saved has them, so their IDs match; then each
    // row Working State holds as Saved did, with the variables Saved gained under
    // Saved's IDs.
    let mut working = destination.working.clone();
    working.services.extend(
        saved
            .services
            .iter()
            .filter(|node| arriving.contains(node.lineage_id.as_str()))
            .cloned(),
    );
    working.volumes.extend(
        saved
            .volumes
            .iter()
            .filter(|node| arriving.contains(node.resource_lineage_id.as_str()))
            .cloned(),
    );
    working.configs.extend(
        saved
            .configs
            .iter()
            .filter(|node| arriving.contains(node.resource_lineage_id.as_str()))
            .cloned(),
    );
    let way = Move::merge(tx, from, &destination)?;
    let old_cells = Cells::of(&old, &way.hostnames.into);
    let mirror = way.plan(&working);
    // A node's cells leave out some of what a draft can change, such as whether a
    // variable is exported, so a removal compares the whole record.
    let (drafted, before) = (
        canonicalize_environment_intent(working.clone()),
        canonicalize_environment_intent(old.clone()),
    );
    let unchanged = |lineage: &str| {
        let service = |intent: &SavedEnvironmentIntent| {
            intent
                .services
                .iter()
                .find(|s| s.lineage_id == lineage)
                .cloned()
        };
        let config = |intent: &SavedEnvironmentIntent| {
            intent
                .configs
                .iter()
                .find(|c| c.resource_lineage_id == lineage)
                .cloned()
        };
        service(&drafted) == service(&before) && config(&drafted) == config(&before)
    };
    let mut staged: BTreeSet<RowId> = mirror
        .rows()
        .iter()
        .filter(|row| {
            picks.contains(&row.id)
                && !arriving.contains(row.id.lineage())
                && matches!(row.verdict, Verdict::Moves { .. })
                && if removed.contains(row.id.lineage()) {
                    unchanged(row.id.lineage())
                } else {
                    row.into == *old_cells.at(&row.id)
                }
        })
        .map(|row| row.id.clone())
        .collect();
    // A draft that still references a removed node keeps it in Working State.
    loop {
        let refused = mirror
            .removal_dependents(&staged, &values)
            .map_err(config)?;
        if refused.is_empty() {
            break;
        }
        staged.retain(|row| !refused.contains_key(row));
    }
    if !staged.is_empty() {
        let next = mirror.apply(&staged, &values).map_err(config)?.next;
        working = with_variable_ids_of(&saved, &working, next);
    }
    let consumed = end_included(tx, &into, Some(&from.summary.id))?;
    if working != destination.working {
        let carried = Carried::of(tx, &from.summary.id, &from.working)?;
        crate::branch::land(
            tx,
            who,
            &mut destination,
            (&from.working, &carried),
            working,
            &staged,
        )?;
    } else if consumed {
        scope::persist_working(tx, &mut destination)?;
    }

    // The pair shares the PR Environment's values: an Edit is the Destination's own
    // change, never one the preview still has to send.
    share(tx, (&into, &from.summary.id), &applied.base)?;
    tx.execute(
        "DELETE FROM config_merge_value WHERE environment_id = ?1 AND destination_id = ?2",
        &[from.summary.id.as_str().into(), into.as_str().into()],
    )?;
    Ok(MergedServices {
        updates: affected(&saved, &picks),
        removals,
    })
}

/// The IDs of the Services of `saved` that `picks` changed: their own rows, a Volume
/// or Config they mount, or a variable they read, through other variables or a
/// mounted Config's files too.
fn affected(saved: &SavedEnvironmentIntent, picks: &BTreeSet<RowId>) -> Vec<String> {
    let lineages: BTreeSet<&str> = picks.iter().map(RowId::lineage).collect();
    // A Service's Private DNS also produces its built-in address variables.
    let mut changed: BTreeSet<(String, String)> = picks
        .iter()
        .flat_map(|pick| {
            let keys = match pick.at() {
                At::Variable(key) => vec![key.clone()],
                At::Setting(Setting::PrivateDns) => {
                    vec![PRIVATE_DOMAIN_KEY.to_owned(), SERVICE_NAME_KEY.to_owned()]
                }
                At::Node
                | At::Data
                | At::Name
                | At::Storage
                | At::File(_)
                | At::Setting(_)
                | At::Mount(_)
                | At::ConfigMount(_) => Vec::new(),
            };
            keys.into_iter().map(|key| (pick.lineage().to_owned(), key))
        })
        .collect();
    let reads = |changed: &BTreeSet<(String, String)>, own: &str, parts: &[ValuePart]| {
        parts.iter().any(|part| match part {
            ValuePart::Ref { owner, key } => {
                let lineage = match owner {
                    ValuePartOwner::Self_ => own,
                    ValuePartOwner::Service { lineage_id } => lineage_id.as_str(),
                };
                changed.contains(&(lineage.to_owned(), key.clone()))
            }
            ValuePart::Text { .. } => false,
        })
    };
    loop {
        let before = changed.len();
        for service in &saved.services {
            for variable in &service.variables {
                if reads(&changed, &service.lineage_id, variable.value.parts()) {
                    changed.insert((service.lineage_id.clone(), variable.key.clone()));
                }
            }
        }
        if changed.len() == before {
            break;
        }
    }
    saved
        .services
        .iter()
        .filter(|service| {
            let own = service.lineage_id.as_str();
            lineages.contains(own)
                || service
                    .variables
                    .iter()
                    .any(|variable| changed.contains(&(own.to_owned(), variable.key.clone())))
                || service.volume_attachments.iter().any(|mount| {
                    saved.volumes.iter().any(|volume| {
                        volume.resource_id == mount.volume_resource_id
                            && lineages.contains(volume.resource_lineage_id.as_str())
                    })
                })
                || service.config_attachments.iter().any(|mount| {
                    saved.configs.iter().any(|config| {
                        config.resource_id == mount.config_resource_id
                            && (lineages.contains(config.resource_lineage_id.as_str())
                                || config
                                    .files
                                    .values()
                                    .any(|file| reads(&changed, own, &file.content)))
                    })
                })
        })
        .map(|service| service.id.clone())
        .collect()
}

/// `next` with each variable `before` lacked under the ID `saved` gave it (by
/// Service lineage and key), so Saved and Working State agree.
fn with_variable_ids_of(
    saved: &SavedEnvironmentIntent,
    before: &SavedEnvironmentIntent,
    mut next: SavedEnvironmentIntent,
) -> SavedEnvironmentIntent {
    for node in &mut next.services {
        let had: BTreeSet<&str> = before
            .services
            .iter()
            .find(|own| own.id == node.id)
            .map(|own| own.variables.iter().map(|v| v.id.as_str()).collect())
            .unwrap_or_default();
        let Some(in_saved) = saved
            .services
            .iter()
            .find(|own| own.lineage_id == node.lineage_id)
        else {
            continue;
        };
        for variable in &mut node.variables {
            if had.contains(variable.id.as_str()) {
                continue;
            }
            if let Some(own) = in_saved.variables.iter().find(|v| v.key == variable.key) {
                variable.id.clone_from(&own.id);
            }
        }
    }
    next
}

/// Forget the Edits of PR Environment `environment`: its pull request retargeted.
pub(crate) fn forget_edits(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_merge_value WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    Ok(())
}
