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
    /// The Organization an operation acts in, which can differ from the one this device
    /// last acted in once a browser has moved the session.
    #[serde(default)]
    organization: Option<String>,
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
    Set,
    Clean,
    Remove,
    Forget,
    Release,
    RemoveMirror,
}

impl Verb {
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Publish => "save",
            Self::Deploy => "deploy",
            Self::Drain => "drain",
            Self::Set => "change",
            Self::Clean => "cleanup",
            Self::Remove => "removal",
            Self::Forget => "forgetting",
            Self::Release => "release",
            Self::RemoveMirror => "mirror removal",
        }
    }

    pub(crate) fn past(self) -> &'static str {
        match self {
            Self::Publish => "saved",
            Self::Deploy => "deployed",
            Self::Drain => "drained",
            Self::Set => "changed",
            Self::Clean => "cleaned",
            Self::Remove => "removed",
            Self::Forget => "forgotten",
            Self::Release => "released",
            Self::RemoveMirror => "removed",
        }
    }

    /// `this change to web-2`, but `this removal of web-1`.
    fn preposition(self) -> &'static str {
        match self {
            Self::Set => "to",
            Self::Publish
            | Self::Deploy
            | Self::Drain
            | Self::Clean
            | Self::Remove
            | Self::Forget
            | Self::Release
            | Self::RemoveMirror => "of",
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

    /// The verb Cloud named, or `Deploy` for a Store write, whose diff names none.
    pub(crate) fn verb(&self) -> Verb {
        match &self.subject {
            Subject::Diff(_) => Verb::Deploy,
            Subject::Operation(operation) => operation.verb,
        }
    }

    /// The name a person types to approve: the Environment's, or the operation's.
    pub(crate) fn name(&self) -> &str {
        match &self.subject {
            Subject::Diff(diff) => diff.environment.name.as_str(),
            Subject::Operation(operation) => &operation.name,
        }
    }

    /// `this deploy to shop/production`, `this removal of web-1`, `this change to web-2`.
    pub(crate) fn what(&self, verb: Verb) -> String {
        let noun = verb.noun();
        match &self.subject {
            Subject::Diff(diff) => format!(
                "this {noun} to {}/{}",
                diff.environment.project, diff.environment.name
            ),
            Subject::Operation(operation) => {
                format!("this {noun} {} {}", verb.preposition(), operation.name)
            }
        }
    }

    pub(crate) fn review(&self, verb: Verb) -> Vec<String> {
        let within = match &self.organization {
            Some(organization) => format!(" in Organization {organization}"),
            None => String::new(),
        };
        let changes_only = !self.effects.is_empty()
            && self
                .effects
                .iter()
                .all(|effect| effect.kind == DestructiveKind::ChangesRole);
        let mut lines = vec![if verb == Verb::Publish {
            format!(
                "The next deploy{within} after this save destroys {}:",
                things(self.effects.len())
            )
        } else if changes_only {
            format!("This {}{within}:", verb.noun())
        } else {
            format!(
                "This {}{within} destroys {}:",
                verb.noun(),
                things(self.effects.len())
            )
        }];
        lines.extend(self.effects.iter().map(|effect| {
            let mark = if effect.kind == DestructiveKind::ChangesRole {
                "~"
            } else {
                "✕"
            };
            format!("  {mark} {}", line(effect))
        }));
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
        DestructiveKind::RemovesMember => format!("removes member {name}"),
        DestructiveKind::ChangesRole => format!("changes the role of {name}"),
        DestructiveKind::RevokesInvite => format!("revokes the invite for {name}"),
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

#[cfg(test)]
mod tests {
    use super::{Asked, Verb};

    fn asked(verb: &str, kind: &str) -> Asked {
        serde_json::from_value(serde_json::json!({
            "approval_id": "a1",
            "approval": "d1",
            "effects": [{ "kind": kind, "name": "bob@example.com", "node": "u1", "path": "members/u1" }],
            "operation": { "verb": verb, "name": "bob@example.com" },
            "organization": "acme",
        }))
        .unwrap()
    }

    #[test]
    fn a_role_change_reads_as_a_change_in_its_organization() {
        let asked = asked("set", "changes_role");
        assert_eq!(
            asked.review(asked.verb()),
            [
                "This change in Organization acme:",
                "  ~ changes the role of bob@example.com",
            ]
        );
    }

    #[test]
    fn a_member_removal_names_its_organization_and_what_it_destroys() {
        let asked = asked("remove", "removes_member");
        assert_eq!(
            asked.review(asked.verb()),
            [
                "This removal in Organization acme destroys 1 thing:",
                "  ✕ removes member bob@example.com",
            ]
        );
    }

    /// Cloud names every operation it gates with one of its `OperationVerb`s; one this CLI
    /// cannot read leaves its `approval_required` refusal unasked.
    #[test]
    fn every_operation_verb_cloud_sends_is_a_verb() {
        let approvals = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../dashboard/src/modules/approvals/approvals.ts");
        let source = std::fs::read_to_string(approvals).unwrap();
        let union = source
            .lines()
            .find_map(|line| line.strip_prefix("export type OperationVerb = "))
            .expect(
                "wants `export type OperationVerb = ...` on one line in dashboard/src/modules/approvals/approvals.ts",
            );
        let verbs: Vec<&str> = union
            .trim_end_matches(';')
            .split('|')
            .map(|verb| verb.trim().trim_matches('"'))
            .collect();
        assert!(verbs.contains(&"drain"), "{union}");
        for verb in verbs {
            assert!(
                serde_json::from_value::<Verb>(serde_json::json!(verb)).is_ok(),
                "Cloud asks approval to {verb}, which `Verb` does not read"
            );
        }
    }
}
