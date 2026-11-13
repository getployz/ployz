//! The tool that stages a Saved revision back: restore all of it, or undo what it
//! changed.

use ployz_core::{RpcError, RpcErrorCode};
use serde_json::{Value, json};

use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool};
use super::refusal::{Call, Refusal, Writes, from_store};
use crate::{HistoryAction, HistoryPreviewQuery, Revision, StageHistory};

tool_input! {
    pub(crate) struct History {
        /// Saved revision number.
        revision: u64,
        /// Reverse only what this revision changed, keeping later ones.
        #[serde(default)]
        undo: bool,
        /// The Working State version a refusal names; required with accept_overwrite.
        #[serde(default)]
        version: Option<String>,
        /// Replace the Staged changes a refusal named.
        #[serde(default)]
        accept_overwrite: bool,
        @place(here),
    }
}

impl Tool for History {
    const NAME: &'static str = "history";
    const DESCRIPTION: &'static str = "Stage a Saved revision's whole configuration, or with undo only the reverse of its changes. Staged; the next deploy asks a human if it removes something. Sealed values and Volume data are not restored.";
    const EFFECT: Effect = Effect::Change;

    fn example() -> Value {
        json!({ "revision": 1, "undo": true })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        stage(self, cx)
    }
}

/// Preview the action on `history.revision`, then stage it pinned to that preview.
fn stage(history: History, cx: &mut Cx<'_>) -> Result<Value, Stop> {
    let action = if history.undo {
        HistoryAction::Undo
    } else {
        HistoryAction::Restore
    };
    if history.accept_overwrite && history.version.is_none() {
        return Err(Refusal::invalid(
            "accept_overwrite needs the version its refusal gave",
            "version",
        )
        .into());
    }
    let environment = cx.scope(&history);
    let revision = Revision(history.revision);
    let preview = cx.one(&HistoryPreviewQuery {
        environment: environment.clone(),
        revision,
        action,
        change: None,
    })?;
    if !history.accept_overwrite && !preview.overwritten.is_empty() {
        return Err(overwrites(cx.call(), preview.overwritten, &preview.version).into());
    }
    let staged = cx.write_with(
        &StageHistory {
            environment,
            revision,
            action,
            change: None,
            version: history.version.unwrap_or(preview.version),
            accept_overwrite: history.accept_overwrite,
        },
        stale,
    )?;
    let not_set = staged
        .not_set
        .iter()
        .map(|variable| format!("{}.env.{}", variable.service, variable.key))
        .collect::<Vec<_>>();
    Ok(json!({
        "at": staged.environment.to_string(),
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

/// A Store refusal, plus for a version that went stale the unpinned call that
/// asks again with the overwrites named fresh.
fn stale(error: RpcError, call: &Call, writes: Writes) -> Refusal {
    let refusal = from_store(error, call, writes);
    let mut input = call.input.clone();
    let Value::Object(fields) = &mut input else {
        return refusal;
    };
    if refusal.code != RpcErrorCode::Conflict || fields.remove("version").is_none() {
        return refusal;
    }
    fields.remove("accept_overwrite");
    refusal.with_next(Call::new(&call.tool, input))
}
