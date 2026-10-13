use std::collections::BTreeMap;

use ployz_core::RpcError;
use ployz_core::config::{SavedEnvironmentIntent, SavedVariableIntent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::id::{Principal, Revision};
use crate::review::{self, NodeChange};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary, revision_param};

use crate::storage::Tx;
use crate::{Actor, error};

/// Saved revisions of one Environment, newest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// How many of the newest revisions to read; defaults to `HISTORY_PAGE`.
    #[serde(default)]
    #[ts(optional)]
    pub limit: Option<u32>,
}

/// How many revisions one History read returns unless asked for more.
pub const HISTORY_PAGE: u32 = 50;

/// The immutable History of an Environment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct HistoryView {
    pub environment: EnvironmentSummary,
    pub revisions: Vec<SavedRevision>,
    /// Whether older revisions exist beyond `limit`.
    pub more: bool,
}

/// One immutable Saved snapshot's metadata and redacted change summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct SavedRevision {
    pub revision: Revision,
    pub message: Option<String>,
    pub saved_by: Option<Principal>,
    /// Seconds since the Unix epoch; absent on legacy snapshots.
    pub saved_at: Option<i64>,
    pub predecessor: Option<Revision>,
    pub changes: Vec<NodeChange>,
    pub total_count: usize,
    /// Change path to how many changes undoing that one change reverses, for
    /// changes the same save linked together. Absent means one.
    pub linked: BTreeMap<String, usize>,
}

/// Restore a complete version, or reverse only what one revision changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HistoryAction {
    Restore,
    Undo,
}

/// Preview a History action against the draft as it is now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct HistoryPreviewQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub revision: Revision,
    pub action: HistoryAction,
    /// Undo only this change of the revision, by its path, and what it links.
    #[serde(default)]
    #[ts(optional)]
    pub change: Option<String>,
}

/// What a History action stages, and exactly which draft fields it overwrites.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct HistoryPreview {
    pub environment: EnvironmentSummary,
    pub revision: Revision,
    pub action: HistoryAction,
    #[ts(optional)]
    pub change: Option<String>,
    /// The exact current Diff version, required when staging this preview.
    pub version: String,
    pub changes: Vec<NodeChange>,
    pub total_count: usize,
    pub overwritten: Vec<String>,
}

/// Stage a reviewed History action without saving or deploying it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StageHistory {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub revision: Revision,
    pub action: HistoryAction,
    #[serde(default)]
    #[ts(optional)]
    pub change: Option<String>,
    pub version: String,
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub accept_overwrite: bool,
}

/// The draft after a History action; Saved State and deployments are unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct HistoryStaged {
    pub environment: EnvironmentSummary,
    pub saved: Option<Revision>,
    pub changed: bool,
    /// Sealed variables the action brought back without a value.
    pub not_set: Vec<NotSetVariable>,
}

/// A sealed variable History brought back sealed without a value: Deploys leave
/// it out until someone sets it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NotSetVariable {
    /// The Service's name.
    pub service: String,
    pub key: String,
}

pub(crate) fn history(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &HistoryQuery,
) -> Result<HistoryView, RpcError> {
    let mut environment = scope::environment(tx, who, &query.environment)?;
    let scrub = review::Scrub::load(tx, &environment.summary.id)?;
    let limit = query.limit.unwrap_or(HISTORY_PAGE).max(1);
    // One extra row is the oldest shown revision's predecessor, and says more exist.
    let mut rows = tx.query(
        "SELECT revision, intent, message, saved_by, saved_at FROM config_saved WHERE environment_id = ?1 ORDER BY revision DESC LIMIT ?2",
        &[
            environment.summary.id.as_str().into(),
            (i64::from(limit) + 1).into(),
        ],
    )?;
    rows.reverse();
    let shown = usize::try_from(limit).unwrap_or(usize::MAX);
    let first = rows.len().saturating_sub(shown);
    let more = first > 0;
    let mut revisions = Vec::with_capacity(rows.len().saturating_sub(first));
    let mut previous = None;
    for (index, row) in rows.iter().enumerate() {
        let revision = Revision(row.number(0, "revision")?);
        let mut intent = row.intent(1, "Saved State")?;
        scrub.apply(revision, &mut intent);
        if index >= first {
            environment.working = intent.clone();
            let changes = previous
                .as_ref()
                .map(|(_, baseline)| review::saved_comparison(&environment, baseline))
                .transpose()?;
            revisions.push(SavedRevision {
                revision,
                message: row.optional_text(2)?.map(str::to_owned),
                saved_by: row.parse_optional(3, "saved actor")?,
                saved_at: row.optional_int(4)?,
                predecessor: previous.as_ref().map(|(revision, _)| *revision),
                changes: changes
                    .as_ref()
                    .map_or_else(Vec::new, |changes| changes.changes.clone()),
                total_count: changes.as_ref().map_or(0, |changes| changes.total_count),
                linked: changes.map(|changes| changes.linked).unwrap_or_default(),
            });
        }
        previous = Some((revision, intent));
    }
    revisions.reverse();
    Ok(HistoryView {
        environment: environment.summary,
        revisions,
        more,
    })
}

pub(crate) fn preview(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &HistoryPreviewQuery,
) -> Result<HistoryPreview, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let reviewed = review::review(tx, &environment)?;
    let prepared = prepare(
        tx,
        &environment,
        &reviewed,
        query.revision,
        query.action,
        query.change.as_deref(),
    )?;
    Ok(preview_of(&environment, &reviewed, &prepared, query))
}

pub(crate) fn stage(
    tx: &mut dyn Tx,
    who: &Actor,
    command: &StageHistory,
) -> Result<HistoryStaged, RpcError> {
    let environment = scope::lock(tx, who, &command.environment)?;
    let reviewed = review::review(tx, &environment)?;
    if command.version != reviewed.view.version {
        return Err(error::conflict(
            "The Environment changed after this History review. Review it again",
            json!({ "diff": reviewed.view }),
        ));
    }
    let prepared = prepare(
        tx,
        &environment,
        &reviewed,
        command.revision,
        command.action,
        command.change.as_deref(),
    )?;
    if !prepared.overwritten().is_empty() && !command.accept_overwrite {
        let query = HistoryPreviewQuery {
            environment: command.environment.clone(),
            revision: command.revision,
            action: command.action,
            change: command.change.clone(),
        };
        return Err(error::confirmation_required(
            "This History action overwrites draft changes",
            json!({ "history_preview": preview_of(&environment, &reviewed, &prepared, &query) }),
        ));
    }
    let not_set = prepared.not_set().to_vec();
    let (environment, changed) = prepared.persist(tx)?;
    Ok(HistoryStaged {
        environment,
        saved: reviewed.view.saved,
        changed,
        not_set,
    })
}

/// The version saved at `revision`, as History shows it.
fn snapshot(
    tx: &mut dyn Tx,
    environment: &Environment,
    scrub: &review::Scrub,
    revision: Revision,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let rows = tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1 AND revision = ?2",
        &[
            environment.summary.id.as_str().into(),
            revision_param(revision)?.into(),
        ],
    )?;
    let mut intent = rows
        .first()
        .ok_or_else(|| {
            error::not_found(
                "No saved version has this revision",
                json!({ "revision": revision }),
            )
        })?
        .intent(0, "Saved State")?;
    scrub.apply(revision, &mut intent);
    Ok(intent)
}

fn prepare(
    tx: &mut dyn Tx,
    environment: &Environment,
    reviewed: &review::Review,
    revision: Revision,
    action: HistoryAction,
    change: Option<&str>,
) -> Result<super::authored::Prepared, RpcError> {
    if action == HistoryAction::Restore && change.is_some() {
        return Err(error::invalid(
            "Only Undo takes a single change; Restore puts back the whole version",
            json!({ "revision": revision }),
        ));
    }
    let scrub = review::Scrub::load(tx, &environment.summary.id)?;
    let selected = snapshot(tx, environment, &scrub, revision)?;
    let previous = match action {
        HistoryAction::Restore => None,
        HistoryAction::Undo => {
            let rows = tx.query("SELECT revision FROM config_saved WHERE environment_id = ?1 AND revision < ?2 ORDER BY revision DESC LIMIT 1", &[environment.summary.id.as_str().into(), revision_param(revision)?.into()])?;
            let predecessor = rows.first().ok_or_else(|| {
                error::conflict(
                    "This saved version has no recorded predecessor to undo",
                    json!({ "revision": revision }),
                )
            })?;
            Some(snapshot(
                tx,
                environment,
                &scrub,
                Revision(predecessor.number(0, "revision")?),
            )?)
        }
    };
    match super::authored::prepare_history(
        tx,
        environment,
        scrub.ever(),
        reviewed.saved.as_ref().map(|saved| &saved.intent),
        &selected,
        previous.as_ref(),
        change,
    ) {
        Err(error) if error.details.get("removed").is_some() => {
            Err(deleted_since(tx, environment, revision, &selected, error)?)
        }
        prepared => prepared,
    }
}
fn preview_of(
    environment: &Environment,
    reviewed: &review::Review,
    prepared: &super::authored::Prepared,
    query: &HistoryPreviewQuery,
) -> HistoryPreview {
    HistoryPreview {
        environment: environment.summary.clone(),
        revision: query.revision,
        action: query.action,
        change: query.change.clone(),
        version: reviewed.view.version.clone(),
        changes: prepared.preview().changes.clone(),
        total_count: prepared.preview().total_count,
        overwritten: prepared.overwritten().to_vec(),
    }
}

/// `error`, from a change to a node deleted since `revision`, as the node's name
/// and the save that deleted it.
fn deleted_since(
    tx: &mut dyn Tx,
    environment: &Environment,
    revision: Revision,
    selected: &SavedEnvironmentIntent,
    error: RpcError,
) -> Result<RpcError, RpcError> {
    let text = |key: &str| error.details.get(key).and_then(serde_json::Value::as_str);
    let (Some(kind), Some(id)) = (text("removed"), text("id")) else {
        return Ok(error);
    };
    let key = text("key").unwrap_or_default();
    fn variables<'a>(
        intent: &'a SavedEnvironmentIntent,
        service: &str,
    ) -> Option<&'a [SavedVariableIntent]> {
        intent
            .services
            .iter()
            .find(|candidate| candidate.id == service)
            .map(|candidate| candidate.variables.as_slice())
    }
    // The node's name in `intent`, and for a variable the ID it has there: a
    // variable found by key is followed by its new ID from then on.
    let name_in = |intent: &SavedEnvironmentIntent,
                   name: &str,
                   variable: &Option<String>|
     -> Option<(String, Option<String>)> {
        let named = |name: String| Some((name, None));
        match kind {
            "service" => intent
                .services
                .iter()
                .find(|service| service.id == id)
                .and_then(|service| named(service.slug.clone())),
            "variable" => {
                let variables = variables(intent, id)?;
                variables
                    .iter()
                    .find(|now| variable.as_ref() == Some(&now.id))
                    .or_else(|| variables.iter().find(|now| now.key == name))
                    .map(|now| (now.key.clone(), Some(now.id.clone())))
            }
            "volume" => intent
                .volumes
                .iter()
                .find(|volume| volume.resource_id == id)
                .and_then(|volume| named(volume.name.clone())),
            _ => intent
                .configs
                .iter()
                .find(|config| config.resource_id == id)
                .and_then(|config| named(config.name.to_string())),
        }
    };
    let Some((mut name, mut variable)) = name_in(selected, key, &None) else {
        return Ok(error);
    };
    let rows = tx.query(
        "SELECT revision, intent FROM config_saved WHERE environment_id = ?1 AND revision > ?2 \
         ORDER BY revision",
        &[
            environment.summary.id.as_str().into(),
            revision_param(revision)?.into(),
        ],
    )?;
    let then = name.clone();
    let mut renamed = None;
    let mut deleted = None;
    for row in &rows {
        if let Some((held, id)) = name_in(&row.intent(1, "Saved State")?, &name, &variable) {
            if held != name {
                renamed = Some(row.int(0)?);
            }
            name = held;
            variable = id;
            deleted = None;
        } else if deleted.is_none() {
            deleted = Some(row.int(0)?);
        }
    }
    // Undo applies to the draft, so a node gone from it is deleted whatever the
    // saves did, and one renamed in it carries its draft name.
    let message = match (name_in(&environment.working, &name, &variable), deleted) {
        (None, Some(revision)) => format!("{name} was deleted in #{revision}"),
        (None, None) => format!("{name} was deleted"),
        (Some((held, _)), _) if held == then => format!("{name} was deleted"),
        (Some((held, _)), _) if held != name => format!("{then} was renamed to {held}"),
        (Some(_), _) => match renamed {
            Some(revision) => format!("{then} was renamed to {name} in #{revision}"),
            None => format!("{then} was renamed to {name}"),
        },
    };
    Ok(error::conflict(message, error.details))
}
