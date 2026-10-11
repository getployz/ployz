//! A human's approval of a Publication that destroys something. Cloud refuses it with
//! `approval_required`; a person at a terminal types the Environment's name, anyone else
//! waits for a human to approve it in Ployz Cloud. Either way the write runs again with
//! the approval.

use std::time::Duration;

use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::{DestructiveEffect, DestructiveKind, DiffView};
use reqwest::Method;
use serde::Deserialize;

use crate::cloud_account::{self, Credential, StoreCallError};
use crate::failure::Failure;

const POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Deserialize)]
pub(crate) struct Asked {
    #[serde(rename = "approval_id")]
    pub(crate) id: String,
    #[serde(rename = "approval")]
    pub(crate) digest: String,
    pub(crate) effects: Vec<DestructiveEffect>,
    pub(crate) diff: DiffView,
}

impl Asked {
    pub(crate) fn of(error: &RpcError) -> Option<Self> {
        if error.code.as_str() != "approval_required" {
            return None;
        }
        serde_json::from_value(error.details.clone()).ok()
    }

    pub(crate) fn review(&self, verb: &str) -> Vec<String> {
        let mut lines = vec![format!(
            "This {verb} destroys {}:",
            things(self.effects.len())
        )];
        lines.extend(
            self.effects
                .iter()
                .map(|effect| format!("  ✕ {}", line(effect))),
        );
        let others = self.diff.total_count.saturating_sub(self.effects.len());
        if others > 0 {
            lines.push(format!(
                "  + {others} other change{}",
                if others == 1 { "" } else { "s" }
            ));
        }
        lines
    }
}

fn things(count: usize) -> String {
    format!("{count} thing{}", if count == 1 { "" } else { "s" })
}

fn line(effect: &DestructiveEffect) -> String {
    let name = &effect.name;
    match effect.kind {
        DestructiveKind::RemovesService => format!("removes Service {name}"),
        DestructiveKind::DeletesVolume => format!("deletes Volume {name} and its data"),
        DestructiveKind::DetachesVolume => match effect.path.split_once(".mounts.") {
            Some((service, _)) => format!("detaches Volume {name} from {service}"),
            None => format!("detaches Volume {name}"),
        },
        DestructiveKind::RemovesDomain => format!("removes domain {name}"),
    }
}

pub(crate) fn settle(
    runtime: &tokio::runtime::Runtime,
    credential: &Credential,
    verb: &str,
    asked: &Asked,
    retry: String,
) -> Result<String, StoreCallError> {
    for line in asked.review(verb) {
        crate::ui::note(line);
    }
    if crate::ui::can_prompt() {
        let environment = asked.diff.environment.name.as_str();
        crate::ui::confirm_name(
            environment,
            || Failure::usage("Approving needs a terminal"),
            &format!("Nothing {verb}ed; approval {} stays pending.", asked.id),
        )
        .map_err(StoreCallError::Stopped)?;
        let approve = serde_json::json!({ "approve": { "digest": asked.digest } });
        return runtime.block_on(async {
            match decide(credential, &asked.id, &approve).await {
                Ok(()) => Ok(asked.id.clone()),
                Err(StoreCallError::Refused(error)) if error.code == RpcErrorCode::Conflict => {
                    Ok(asked.id.clone())
                }
                Err(error) => Err(error),
            }
        });
    }
    let interrupted = crate::cancellation::interrupted()
        .map_err(|error| StoreCallError::Stopped(Failure::from(error)))?;
    crate::ui::note("Waiting for approval in Ployz Cloud…");
    runtime.block_on(async {
        loop {
            tokio::select! {
                () = interrupted.cancelled() => {
                    return Err(StoreCallError::Stopped(
                        Failure::coded(
                            RpcErrorCode::Internal,
                            format!("Stopped waiting; approval {} stays pending in Ployz Cloud.", asked.id),
                        )
                        .interrupted()
                        .hint(crate::ui::Hint::Retry(retry)),
                    ));
                }
                read = status(credential, &asked.id) => {
                    if read? != Status::Pending {
                        return Ok(asked.id.clone());
                    }
                }
            }
            tokio::select! {
                () = interrupted.cancelled() => {}
                () = tokio::time::sleep(POLL) => {}
            }
        }
    })
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Pending,
    Approved,
    Denied,
    Superseded,
}

async fn status(credential: &Credential, id: &str) -> Result<Status, StoreCallError> {
    #[derive(Deserialize)]
    struct Read {
        approval: Row,
    }
    #[derive(Deserialize)]
    struct Row {
        status: Status,
    }
    let read: Read =
        cloud_account::refusable(credential, Method::GET, &format!("approvals/{id}"), None).await?;
    Ok(read.approval.status)
}

/// Cloud refuses `conflict` once the approval was decided otherwise or its plan changed.
pub(crate) async fn decide(
    credential: &Credential,
    id: &str,
    decision: &serde_json::Value,
) -> Result<(), StoreCallError> {
    cloud_account::refusable::<serde_json::Value>(
        credential,
        Method::POST,
        &format!("approvals/{id}"),
        Some(decision),
    )
    .await?;
    Ok(())
}
