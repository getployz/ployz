//! Tools that stage a Saved revision back: restore all of it, or undo what it
//! changed.

use ployz_core::RpcErrorCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at};
use super::refusal::{Call, Refusal};
use super::release::lines;
use crate::catalog::NODE_NAME;
use crate::{
    EnvironmentName, HistoryAction, HistoryPreviewQuery, ProjectName, Revision, StageHistory,
};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct History {
    /// Saved revision number. review shows the latest as saved_revision.
    revision: u64,
    /// Show what this stages and which Staged changes it overwrites, without writing.
    #[serde(default)]
    preview: bool,
    /// Only if review still shows this version. Required with accept_overwrite.
    #[serde(default)]
    version: Option<String>,
    /// Only from a refusal, when the member accepted losing the Staged changes it named.
    #[serde(default)]
    accept_overwrite: bool,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

#[derive(Deserialize, JsonSchema)]
pub(crate) struct HistoryRestore(History);

#[derive(Deserialize, JsonSchema)]
pub(crate) struct HistoryUndo(History);

impl Tool for HistoryRestore {
    const NAME: &'static str = "history_restore";
    const DESCRIPTION: &'static str = "Stage a Saved revision's whole configuration, reversing every later one. Staged: review shows it, discard undoes it; publish or deploy asks a human if it removes something. Sealed values and Volume data do not come back.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "revision": 1, "preview": true })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        stage(self.0, HistoryAction::Restore, cx)
    }
}

impl Tool for HistoryUndo {
    const NAME: &'static str = "history_undo";
    const DESCRIPTION: &'static str = "Stage the reverse of what one Saved revision changed, keeping later ones. Staged: review shows it, discard undoes it; publish or deploy asks a human if it removes something. Sealed values and Volume data do not come back.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "revision": 2 })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        stage(self.0, HistoryAction::Undo, cx)
    }
}

/// Preview `action` on `history.revision`, then stage it pinned to that preview.
fn stage(history: History, action: HistoryAction, cx: &mut Cx<'_>) -> Result<Value, Stop> {
    if history.accept_overwrite && history.version.is_none() {
        return Err(Refusal::invalid(
            "accept_overwrite needs the version its refusal gave",
            "version",
        )
        .into());
    }
    let environment = cx.scope(history.project.as_ref(), history.environment.as_ref());
    let revision = Revision(history.revision);
    let preview = cx.one(&HistoryPreviewQuery {
        environment: environment.clone(),
        revision,
        action,
        change: None,
    })?;
    if history.preview {
        return Ok(json!({
            "at": at(&preview.environment),
            "revision": revision.0,
            "version": preview.version,
            "lines": preview.changes.iter().flat_map(lines).collect::<Vec<_>>(),
            "overwrites": preview.overwritten,
        }));
    }
    if !history.accept_overwrite && !preview.overwritten.is_empty() {
        return Err(overwrites(cx.call(), preview.overwritten, &preview.version).into());
    }
    let staged = cx.write(&StageHistory {
        environment,
        revision,
        action,
        change: None,
        version: history.version.unwrap_or(preview.version),
        accept_overwrite: history.accept_overwrite,
    })?;
    let not_set = staged
        .not_set
        .iter()
        .map(|variable| format!("{}.env.{}", variable.service, variable.key))
        .collect::<Vec<_>>();
    Ok(json!({
        "at": at(&staged.environment),
        "revision": revision.0,
        "changed": staged.changed,
        "not_set": not_set,
    }))
}

/// The confirmation a staging that overwrites Staged changes needs, its retry
/// pinned to the version whose overwrites it names.
fn overwrites(call: &Call, overwritten: Vec<String>, version: &str) -> Refusal {
    let mut input = call.input.clone();
    if let Value::Object(fields) = &mut input {
        fields.insert("version".into(), version.into());
        fields.insert("accept_overwrite".into(), true.into());
    }
    Refusal::new(
        RpcErrorCode::ConfirmationRequired,
        "This overwrites Staged changes; retry only if the member accepts losing them",
    )
    .with_detail("overwrites", json!(overwritten))
    .with_retry(Call::new(&call.tool, input))
}
