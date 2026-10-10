use std::collections::BTreeSet;

use ployz_core::RpcError;
use ployz_core::config::{
    EnvironmentNodeType, ReviewLifecycleKind, SavedEnvironmentIntent, SavedVariableValue,
    canonicalize_environment_intent,
};
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
    let review = review::review(tx, &environment)?;
    let target = candidate(tx, &environment, query.revision, query.action)?;
    preview_of(&environment, &review, &target, query.revision, query.action)
}

pub(crate) fn stage(
    tx: &mut dyn Tx,
    who: &Actor,
    command: &StageHistory,
) -> Result<HistoryStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &command.environment)?;
    let reviewed = review::review(tx, &environment)?;
    if command.version != reviewed.view.version {
        return Err(error::conflict(
            "The Environment changed after this History review. Review it again",
            json!({ "diff": reviewed.view }),
        ));
    }
    let target = candidate(tx, &environment, command.revision, command.action)?;
    let preview = preview_of(
        &environment,
        &reviewed,
        &target,
        command.revision,
        command.action,
    )?;
    if !preview.overwritten.is_empty() && !command.accept_overwrite {
        return Err(error::confirmation_required(
            "This History action overwrites draft changes",
            json!({ "history_preview": preview }),
        ));
    }
    let changed = canonicalize_environment_intent(environment.working.clone()) != target;
    if changed {
        crate::branch::rewind(tx, &environment.summary.id, (&environment.working, &target))?;
        let restored = snapshot(tx, &environment, command.revision)?;
        environment.working = target;
        scope::save_working_from(tx, &mut environment, Some(&restored))?;
    }
    Ok(HistoryStaged {
        environment: environment.summary,
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

fn candidate(
    tx: &mut dyn Tx,
    environment: &Environment,
    revision: Revision,
    action: HistoryAction,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let selected = snapshot(tx, environment, revision)?;
    let target = match action {
        HistoryAction::Restore => selected.clone(),
        HistoryAction::Undo => {
            let rows = tx.query("SELECT revision, intent FROM config_saved WHERE environment_id = ?1 AND revision < ?2 ORDER BY revision DESC LIMIT 1", &[environment.summary.id.as_str().into(), revision_param(revision)?.into()])?;
            let previous = rows
                .first()
                .ok_or_else(|| {
                    error::conflict(
                        "This saved version has no recorded predecessor to undo",
                        json!({ "revision": revision }),
                    )
                })?
                .intent(1, "Saved State")?;
            undo(environment, &selected, &previous)?
        }
    };
    prevent_plaintext(&environment.working, &target)?;
    let mut candidate = Environment {
        summary: environment.summary.clone(),
        working: target,
        live: environment.live.clone(),
    };
    scope::validate_and_refresh_working_from(tx, &mut candidate, Some(&selected))?;
    crate::volume::check_storage(tx, &environment.summary.id, &candidate.working)?;
    Ok(canonicalize_environment_intent(candidate.working))
}

fn undo(
    environment: &Environment,
    selected: &SavedEnvironmentIntent,
    previous: &SavedEnvironmentIntent,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let mut at = Environment {
        summary: environment.summary.clone(),
        working: selected.clone(),
        live: environment.live.clone(),
    };
    let changes = review::saved_comparison(&at, previous)?;
    at.working = environment.working.clone();
    for change in changes.changes {
        review::publish::restore_change_into(&mut at.working, previous, selected, &change)?;
    }
    Ok(at.working)
}

pub(crate) fn prevent_plaintext(
    current: &SavedEnvironmentIntent,
    target: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    for service in &current.services {
        let Some(restored) = target
            .services
            .iter()
            .find(|restored| restored.id == service.id)
        else {
            continue;
        };
        for variable in &service.variables {
            if matches!(
                variable.value,
                SavedVariableValue::Secret { .. } | SavedVariableValue::SecretWithoutValue
            ) && restored.variables.iter().any(|next| {
                next.key == variable.key
                    && matches!(
                        next.value,
                        SavedVariableValue::Literal { .. } | SavedVariableValue::Template { .. }
                    )
            }) {
                return Err(error::conflict(
                    "A secret cannot become plain text through History",
                    json!({ "path": format!("{}.env.{}", restored.slug, variable.key) }),
                ));
            }
        }
    }
    Ok(())
}

fn preview_of(
    environment: &Environment,
    reviewed: &review::Review,
    target: &SavedEnvironmentIntent,
    revision: Revision,
    action: HistoryAction,
) -> Result<HistoryPreview, RpcError> {
    let candidate = Environment {
        summary: environment.summary.clone(),
        working: target.clone(),
        live: environment.live.clone(),
    };
    let changes = review::saved_comparison(&candidate, &environment.working)?;
    let mut overwritten = BTreeSet::new();
    for change in &changes.changes {
        let Some(draft) = reviewed
            .view
            .draft_changes
            .iter()
            .find(|draft| draft.node == change.node)
        else {
            continue;
        };
        if draft.lifecycle != ReviewLifecycleKind::Update {
            overwritten.insert(node_path(draft));
        } else if change.lifecycle != ReviewLifecycleKind::Update {
            overwritten.extend(draft.settings.iter().map(|row| row.path.clone()));
        } else {
            let renamed = format!("{}.name", draft.name);
            for row in &change.settings {
                if row
                    .row
                    .as_ref()
                    .is_some_and(|row| matches!(row.at(), ployz_core::config::At::Variable(_)))
                {
                    continue;
                }
                if let Some(draft) = draft.settings.iter().find(|draft| {
                    review::same_setting(draft, row)
                        || (draft.path == renamed && row.path == format!("{}.name", change.name))
                }) {
                    overwritten.insert(draft.path.clone());
                }
            }
        }
    }
    if let Some(saved) = &reviewed.saved {
        for service in &environment.working.services {
            let Some(baseline) = saved
                .intent
                .services
                .iter()
                .find(|baseline| baseline.id == service.id)
            else {
                continue;
            };
            let Some(restored) = target
                .services
                .iter()
                .find(|restored| restored.id == service.id)
            else {
                continue;
            };
            let keys: BTreeSet<_> = baseline
                .variables
                .iter()
                .chain(&service.variables)
                .chain(&restored.variables)
                .map(|variable| &variable.key)
                .collect();
            for key in keys {
                let before = baseline
                    .variables
                    .iter()
                    .find(|variable| variable.key == *key);
                let current = service
                    .variables
                    .iter()
                    .find(|variable| variable.key == *key);
                let after = restored
                    .variables
                    .iter()
                    .find(|variable| variable.key == *key);
                let same_value =
                    |left: Option<&ployz_core::config::SavedVariableIntent>,
                     right: Option<&ployz_core::config::SavedVariableIntent>| {
                        match (left, right) {
                            (Some(left), Some(right)) => {
                                left.id == right.id
                                    && left.value == right.value
                                    && left.value_fingerprint == right.value_fingerprint
                            }
                            (None, None) => true,
                            _ => false,
                        }
                    };
                if !same_value(before, current) && !same_value(after, current) {
                    overwritten.insert(format!("{}.env.{key}", service.slug));
                }
                if let Some(current) = current {
                    if before.is_some_and(|before| before.description != current.description)
                        && after.is_some_and(|after| after.description != current.description)
                    {
                        overwritten.insert(format!("{}.env.{key}.description", service.slug));
                    }
                    if before.is_some_and(|before| before.exported != current.exported)
                        && after.is_some_and(|after| after.exported != current.exported)
                    {
                        overwritten.insert(format!("{}.env.{key}.exported", service.slug));
                    }
                }
            }
        }
    }
    Ok(HistoryPreview {
        environment: environment.summary.clone(),
        revision,
        action,
        version: reviewed.view.version.clone(),
        changes: changes.changes,
        total_count: changes.total_count,
        overwritten: overwritten.into_iter().collect(),
    })
}

fn node_path(change: &NodeChange) -> String {
    match change.node.node_type {
        EnvironmentNodeType::Service => change.name.clone(),
        EnvironmentNodeType::Volume => format!("volumes.{}", change.name),
        EnvironmentNodeType::Config => format!("configs.@{}", change.node.id),
    }
}
