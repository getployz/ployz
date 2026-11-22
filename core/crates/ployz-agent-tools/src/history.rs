//! The tool that stages a Saved revision back: restore all of it, or undo what it
//! changed.

use ployz_core::{RpcError, RpcErrorCode};
use serde_json::{Value, json};

use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool};
use super::refusal::{Call, Refusal, Writes, from_store};
use ployz_store::{HistoryAction, HistoryPreviewQuery, Revision, StageHistory};

tool_input! {
    pub(crate) struct History {
        /// The Saved revision's number, e.g. `12`, as a review or a Deployment shows it.
        revision: u64,
        /// Set true to reverse only what this revision changed, keeping later changes. By default its whole configuration is restored.
        #[serde(default)]
        undo: bool,
        /// The version the refusal gave. Fails if what it covers changed since.
        #[serde(default)]
        version: Option<String>,
        /// Replace the staged changes a refusal listed, once the member accepts losing them. Needs that refusal's version.
        #[serde(default)]
        accept_overwrite: bool,
        @place(here),
    }
}

impl Tool for History {
    const NAME: &'static str = "history";
    const TITLE: &'static str = "Restore or undo a Saved revision";
    const DESCRIPTION: &'static str = "Stage a past Saved revision back: its whole configuration, or with undo just the reverse of what it changed. Nothing ships until the next deploy. Sealed values and Volume data are not restored. Returns whether anything changed and which sealed variables are left unset.";
    // Sent again, the draft already holds what the action stages, so nothing changes.
    const EFFECT: Effect = Effect::Change { idempotent: true };

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
