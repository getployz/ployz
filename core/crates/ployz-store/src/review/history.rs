use ployz_core::RpcError;
use ployz_core::config::SavedEnvironmentIntent;
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
}

/// The immutable History of an Environment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct HistoryView {
    pub environment: EnvironmentSummary,
    pub revisions: Vec<SavedRevision>,
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
}

/// What a History action stages, and exactly which draft fields it overwrites.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct HistoryPreview {
    pub environment: EnvironmentSummary,
    pub revision: Revision,
    pub action: HistoryAction,
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
}

pub(crate) fn history(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &HistoryQuery,
) -> Result<HistoryView, RpcError> {
    let mut environment = scope::environment(tx, who, &query.environment)?;
    let rows = tx.query(
        "SELECT revision, intent, message, saved_by, saved_at FROM config_saved WHERE environment_id = ?1 ORDER BY revision",
        &[environment.summary.id.as_str().into()],
    )?;
    let mut revisions = Vec::with_capacity(rows.len());
    let mut previous = None;
    for row in rows {
        let revision = Revision(row.number(0, "revision")?);
        let intent = row.intent(1, "Saved State")?;
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
        });
        previous = Some((revision, intent));
    }
    revisions.reverse();
    Ok(HistoryView {
        environment: environment.summary,
        revisions,
    })
}

pub(crate) fn preview(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &HistoryPreviewQuery,
) -> Result<HistoryPreview, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let reviewed = review::review(tx, &environment)?;
    let prepared = prepare(tx, &environment, &reviewed, query.revision, query.action)?;
    Ok(preview_of(
        &environment,
        &reviewed,
        &prepared,
        query.revision,
        query.action,
    ))
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
    )?;
    if !prepared.overwritten().is_empty() && !command.accept_overwrite {
        return Err(error::confirmation_required(
            "This History action overwrites draft changes",
            json!({ "history_preview": preview_of(&environment, &reviewed, &prepared, command.revision, command.action) }),
        ));
    }
    let (environment, changed) = prepared.persist(tx)?;
    Ok(HistoryStaged {
        environment,
        saved: reviewed.view.saved,
        changed,
    })
}

fn snapshot(
    tx: &mut dyn Tx,
    environment: &Environment,
    revision: Revision,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let rows = tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1 AND revision = ?2",
        &[
            environment.summary.id.as_str().into(),
            revision_param(revision)?.into(),
        ],
    )?;
    rows.first()
        .ok_or_else(|| {
            error::not_found(
                "No saved version has this revision",
                json!({ "revision": revision }),
            )
        })?
        .intent(0, "Saved State")
}

fn prepare(
    tx: &mut dyn Tx,
    environment: &Environment,
    reviewed: &review::Review,
    revision: Revision,
    action: HistoryAction,
) -> Result<super::authored::Prepared, RpcError> {
    let selected = snapshot(tx, environment, revision)?;
    let previous = match action {
        HistoryAction::Restore => None,
        HistoryAction::Undo => {
            let rows = tx.query("SELECT revision, intent FROM config_saved WHERE environment_id = ?1 AND revision < ?2 ORDER BY revision DESC LIMIT 1", &[environment.summary.id.as_str().into(), revision_param(revision)?.into()])?;
            Some(
                rows.first()
                    .ok_or_else(|| {
                        error::conflict(
                            "This saved version has no recorded predecessor to undo",
                            json!({ "revision": revision }),
                        )
                    })?
                    .intent(1, "Saved State")?,
            )
        }
    };
    super::authored::prepare_history(
        tx,
        environment,
        reviewed.saved.as_ref().map(|saved| &saved.intent),
        &selected,
        previous.as_ref(),
    )
}
fn preview_of(
    environment: &Environment,
    reviewed: &review::Review,
    prepared: &super::authored::Prepared,
    revision: Revision,
    action: HistoryAction,
) -> HistoryPreview {
    HistoryPreview {
        environment: environment.summary.clone(),
        revision,
        action,
        version: reviewed.view.version.clone(),
        changes: prepared.preview().changes.clone(),
        total_count: prepared.preview().total_count,
        overwritten: prepared.overwritten().to_vec(),
    }
}
