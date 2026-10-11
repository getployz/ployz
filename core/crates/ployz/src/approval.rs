//! A human's approval of a write or an operation that destroys something. Cloud refuses it
//! with `approval_required`; a person at a terminal types the Environment's or the
//! operation's name, anyone else waits for a human to approve it in Ployz Cloud. Either way
//! the request runs again with the approval.

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
    #[serde(flatten)]
    pub(crate) subject: Subject,
}

/// What the approval covers: a Store write, which carries its diff, or an operation on one
/// named thing.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Subject {
    Diff(Box<DiffView>),
    Operation(Operation),
}

#[derive(Debug, Deserialize)]
pub(crate) struct Operation {
    pub(crate) verb: Verb,
    name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verb {
    Publish,
    Deploy,
    Drain,
    Clean,
    Remove,
}

impl Verb {
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Deploy => "deploy",
            Self::Drain => "drain",
            Self::Clean => "cleanup",
            Self::Remove => "removal",
        }
    }

    pub(crate) fn past(self) -> &'static str {
        match self {
            Self::Publish => "published",
            Self::Deploy => "deployed",
            Self::Drain => "drained",
            Self::Clean => "cleaned",
            Self::Remove => "removed",
        }
    }
}

impl Asked {
    pub(crate) fn of(error: &RpcError) -> Option<Self> {
        if error.code.as_str() != "approval_required" {
            return None;
        }
        serde_json::from_value(error.details.clone()).ok()
    }

    /// The name a person types to approve: the Environment's, or the operation's.
    pub(crate) fn name(&self) -> &str {
        match &self.subject {
            Subject::Diff(diff) => diff.environment.name.as_str(),
            Subject::Operation(operation) => &operation.name,
        }
    }

    /// `this deploy to production`, `this removal of web-1`.
    pub(crate) fn what(&self, verb: Verb) -> String {
        let noun = verb.noun();
        match &self.subject {
            Subject::Diff(diff) => format!("this {noun} to {}", diff.environment.name),
            Subject::Operation(operation) => format!("this {noun} of {}", operation.name),
        }
    }

    pub(crate) fn review(&self, verb: Verb) -> Vec<String> {
        let mut lines = vec![format!(
            "This {} destroys {}:",
            verb.noun(),
            things(self.effects.len())
        )];
        lines.extend(
            self.effects
                .iter()
                .map(|effect| format!("  ✕ {}", line(effect))),
        );
        let others = match &self.subject {
            Subject::Diff(diff) => diff.total_count.saturating_sub(self.effects.len()),
            Subject::Operation(_) => 0,
        };
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
        DestructiveKind::RemovesServer => format!("removes Server {name}"),
    }
}

pub(crate) fn settle(
    runtime: &tokio::runtime::Runtime,
    credential: &Credential,
    verb: Verb,
    asked: &Asked,
    retry: String,
) -> Result<String, StoreCallError> {
    for line in asked.review(verb) {
        crate::ui::note(line);
    }
    if crate::ui::can_prompt() {
        crate::ui::confirm_name(
            asked.name(),
            || Failure::usage("Approving needs a terminal"),
            &format!(
                "Nothing {}; the approval stays pending in Ployz Cloud.",
                verb.past()
            ),
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
    crate::ui::note_inline("Waiting for approval in Ployz Cloud… ");
    let settled = runtime.block_on(async {
        loop {
            tokio::select! {
                () = interrupted.cancelled() => {
                    return Err(StoreCallError::Stopped(
                        Failure::coded(
                            RpcErrorCode::Internal,
                            "Stopped waiting; the approval stays pending in Ployz Cloud.",
                        )
                        .interrupted()
                        .hint(crate::ui::Hint::Retry(retry)),
                    ));
                }
                read = status(credential, &asked.id) => {
                    let status = read?;
                    if status != Status::Pending {
                        return Ok(status);
                    }
                }
            }
            tokio::select! {
                () = interrupted.cancelled() => {}
                () = tokio::time::sleep(POLL) => {}
            }
        }
    });
    crate::ui::note(match &settled {
        Ok(Status::Approved) => "approved.",
        Ok(Status::Denied) => "denied.",
        Ok(Status::Superseded) => "superseded; asking again.",
        Ok(Status::Pending) | Err(_) => "",
    });
    settled.map(|_| asked.id.clone())
}

/// Run `attempt` with `approval` until Cloud stops asking for one: each time it asks,
/// [`settle`] gets a human's answer and `attempt` runs again carrying it. Under `--json`
/// nothing asks; the refusal comes back naming `retry` of the approval's id.
pub(crate) fn approved<T>(
    runtime: &tokio::runtime::Runtime,
    credential: &Credential,
    verb: Verb,
    mut approval: Option<String>,
    retry: impl Fn(&str) -> String,
    mut attempt: impl AsyncFnMut(Option<&str>) -> Result<T, StoreCallError>,
) -> Result<T, Failure> {
    loop {
        let refused = match runtime.block_on(attempt(approval.as_deref())) {
            Err(StoreCallError::Refused(refused)) => refused,
            other => return other.map_err(Failure::from),
        };
        let Some(asked) = Asked::of(&refused) else {
            return Err(refused.into());
        };
        if crate::ui::json() {
            return Err(Failure::from(refused).hint(crate::ui::Hint::Retry(retry(&asked.id))));
        }
        approval = Some(settle(runtime, credential, verb, &asked, retry(&asked.id))?);
    }
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
