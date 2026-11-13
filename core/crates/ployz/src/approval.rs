//! A human's approval of a write or an operation that destroys something. Cloud refuses it
//! with `approval_required`; a person at a terminal types the Environment's or the
//! operation's name, anyone else waits for a human to approve it in Ployz Cloud. Either way
//! the request runs again with the approval.

use std::fmt::Display;
use std::time::Duration;

use chrono::{Local, SecondsFormat, TimeZone};
use ployz_core::{Hint, RpcError, RpcErrorCode};
use ployz_store::{
    ApprovalOperation, ApprovalRequired, ApprovalSubject, DestructiveEffect, DestructiveKind,
    DiffView, OperationVerb,
};
use reqwest::Method;
use serde::Deserialize;

use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::LoginError;
use crate::failure::Failure;

const POLL: Duration = Duration::from_secs(1);

/// What the approval covers: a Store write, which carries its diff, or an operation on one
/// named thing.
pub(crate) type Subject = ApprovalSubject<Box<DiffView>>;

#[derive(Debug, Deserialize)]
#[serde(try_from = "ApprovalRequired<Box<DiffView>>")]
pub(crate) struct Asked {
    pub(crate) id: String,
    pub(crate) required: ApprovalRequired<Box<DiffView>>,
    /// Cloud's reason for asking, such as that the Environment is Protected.
    reason: String,
}

impl TryFrom<ApprovalRequired<Box<DiffView>>> for Asked {
    type Error = &'static str;

    fn try_from(required: ApprovalRequired<Box<DiffView>>) -> Result<Self, Self::Error> {
        let id = required.id.clone().ok_or("an approval names its id")?;
        Ok(Self {
            id,
            required,
            reason: String::new(),
        })
    }
}

impl Asked {
    fn for_someone_else(&self) -> bool {
        !matches!(
            self.required.protected,
            None | Some(serde_json::Value::Null | serde_json::Value::Bool(false))
        )
    }
}

fn renamed_addresses(operation: &ApprovalOperation) -> String {
    let to = format!("*.{}.ployz.internal", operation.name);
    match operation.renamed_from() {
        Some(from) => format!("*.{from}.ployz.internal → {to}"),
        None => to,
    }
}

/// The CLI's words for an approval: Cloud's operations, and the Store writes the CLI
/// names itself, whose diff names no verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    Publish,
    Deploy,
    Retry,
    Operation(OperationVerb),
}

impl From<OperationVerb> for Verb {
    fn from(verb: OperationVerb) -> Self {
        Self::Operation(verb)
    }
}

impl Verb {
    pub(crate) fn noun(self) -> &'static str {
        match self {
            Self::Publish => "save",
            Self::Deploy => "deploy",
            Self::Retry => "redeploy",
            Self::Operation(verb) => match verb {
                OperationVerb::Drain => "drain",
                OperationVerb::Set => "change",
                OperationVerb::Clean => "cleanup",
                OperationVerb::Remove => "removal",
                OperationVerb::Forget => "forgetting",
                OperationVerb::Release => "release",
                OperationVerb::Mirror => "mirror",
                OperationVerb::Sync => "full sync",
                OperationVerb::RemoveMirror => "mirror removal",
                OperationVerb::Stop => "stop",
                OperationVerb::Restart => "restart",
                OperationVerb::Unprotect => "unprotecting",
                OperationVerb::Move => "move",
                OperationVerb::Restore => "restore",
                OperationVerb::Resize => "resize",
                OperationVerb::SetClusterName => "rename",
                OperationVerb::SetIngressDoors => "door change",
                OperationVerb::SetPrivateAccess => "Private Access change",
            },
        }
    }

    pub(crate) fn past(self) -> &'static str {
        match self {
            Self::Publish => "saved",
            Self::Deploy => "deployed",
            Self::Retry => "retried",
            Self::Operation(verb) => match verb {
                OperationVerb::Drain => "drained",
                OperationVerb::Set => "changed",
                OperationVerb::Clean => "cleaned",
                OperationVerb::Remove => "removed",
                OperationVerb::Forget => "forgotten",
                OperationVerb::Release => "released",
                OperationVerb::Mirror => "mirrored",
                OperationVerb::Sync => "synced",
                OperationVerb::RemoveMirror => "removed",
                OperationVerb::Stop => "stopped",
                OperationVerb::Restart => "restarted",
                OperationVerb::Unprotect => "unprotected",
                OperationVerb::Move => "moved",
                OperationVerb::Restore => "restored",
                OperationVerb::Resize => "resized",
                OperationVerb::SetClusterName => "renamed",
                OperationVerb::SetIngressDoors | OperationVerb::SetPrivateAccess => "changed",
            },
        }
    }

    /// `this change to web-2`, but `this removal of web-1`.
    fn preposition(self) -> &'static str {
        match self {
            Self::Operation(
                OperationVerb::Set
                | OperationVerb::SetClusterName
                | OperationVerb::SetIngressDoors
                | OperationVerb::SetPrivateAccess,
            ) => "to",
            Self::Publish
            | Self::Deploy
            | Self::Retry
            | Self::Operation(
                OperationVerb::Drain
                | OperationVerb::Clean
                | OperationVerb::Remove
                | OperationVerb::Forget
                | OperationVerb::Release
                | OperationVerb::Mirror
                | OperationVerb::Sync
                | OperationVerb::RemoveMirror
                | OperationVerb::Stop
                | OperationVerb::Restart
                | OperationVerb::Unprotect
                | OperationVerb::Move
                | OperationVerb::Restore
                | OperationVerb::Resize,
            ) => "of",
        }
    }
}

impl Asked {
    pub(crate) fn of(error: &RpcError) -> Option<Self> {
        if error.code.as_str() != ployz_core::APPROVAL_REQUIRED_CODE {
            return None;
        }
        let asked: Self = serde_json::from_value(error.details.clone()).ok()?;
        Some(Self {
            reason: error.message.clone(),
            ..asked
        })
    }

    /// The verb Cloud named, or `Deploy` for a Store write, whose diff names none.
    pub(crate) fn verb(&self) -> Verb {
        match &self.required.subject {
            Subject::Diff(_) => Verb::Deploy,
            Subject::Operation(operation) => Verb::Operation(operation.verb),
        }
    }

    /// The name a person types to approve: the Environment's, or the operation's.
    pub(crate) fn name(&self) -> &str {
        match &self.required.subject {
            Subject::Diff(diff) => diff.environment.name.as_str(),
            Subject::Operation(operation) => &operation.name,
        }
    }

    /// `this deploy to shop/production`, `this removal of web-1`, `this change to web-2`.
    pub(crate) fn what(&self, verb: Verb) -> String {
        let noun = verb.noun();
        match &self.required.subject {
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
        self.review_in(verb, &Local)
    }

    fn review_in<Zone: TimeZone>(&self, verb: Verb, zone: &Zone) -> Vec<String>
    where
        Zone::Offset: Display,
    {
        let within = match &self.required.organization {
            Some(organization) => format!(" in Organization {organization}"),
            None => String::new(),
        };
        let loses_volume_access = matches!(&self.required.subject, Subject::Operation(operation) if operation.loses_volume_access());
        let changes_only = self
            .required
            .effects
            .iter()
            .all(|effect| !destroys(effect.kind));
        // A redeploy ships what its Deployment froze, so its card names that Deployment.
        let (verb, within) = match &self.required.redeploy {
            Some(redeploy) => (Verb::Retry, format!(" of #{}{within}", redeploy.deployment)),
            None => (verb, within),
        };
        let mut lines = vec![if verb == Verb::Publish && changes_only {
            format!("The next deploy{within} after this save:")
        } else if verb == Verb::Publish {
            format!(
                "The next deploy{within} after this save destroys {}:",
                things(self.required.effects.len())
            )
        } else if changes_only || loses_volume_access {
            format!("This {}{within}:", verb.noun())
        } else {
            format!(
                "This {}{within} destroys {}:",
                verb.noun(),
                things(self.required.effects.len())
            )
        }];
        for effect in &self.required.effects {
            let mark = if destroys(effect.kind) { "✕" } else { "~" };
            let line = if loses_volume_access && effect.kind == DestructiveKind::DeletesVolume {
                format!(
                    "loses access to Volume {}; its data stays on the Server's disk until wiped. \
                     Wipe or reinstall the disk to erase it; sudo ployz-uninstall keeps the data.",
                    effect.name
                )
            } else {
                line(effect)
            };
            lines.push(format!("  {mark} {line}"));
            if let Some(lost) = &effect.lost_after {
                lines.push(format!(
                    "    Changes after {} are not in {}",
                    moment(lost.at, zone),
                    lost.restored
                ));
            }
        }
        if let Subject::Operation(operation) = &self.required.subject
            && operation.verb == OperationVerb::SetClusterName
        {
            lines.push(format!("  ~ {}", renamed_addresses(operation)));
        }
        if let Some(redeploy) = &self.required.redeploy
            && let Some(undoes) = deployments(&redeploy.rolls_back)
        {
            lines.push(format!(
                "  ~ rolls back to #{}, undoing {undoes}",
                redeploy.deployment
            ));
        }
        if self.required.prunes {
            lines.push("  ✕ removes any other Service running in this environment".to_owned());
            if !self.required.dropped.is_empty() {
                lines.push(format!(
                    "    couldn't check every server: large packets to {} are dropped on the mesh path",
                    self.required.dropped.join(", ")
                ));
            }
        }
        let others = match &self.required.subject {
            Subject::Diff(diff) => diff.total_count.saturating_sub(self.required.effects.len()),
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

/// `#5`, `#5 and #6`, `#5, #6 and #7`; None for no Deployments.
pub(crate) fn deployments(numbers: &[u64]) -> Option<String> {
    let (last, rest) = numbers.split_last()?;
    let named = |number: &u64| format!("#{number}");
    Some(match rest {
        [] => named(last),
        _ => format!(
            "{} and {}",
            rest.iter().map(named).collect::<Vec<_>>().join(", "),
            named(last)
        ),
    })
}

/// A role change, a stop, a restart or an unprotect interrupts or alters something; it leaves nothing gone.
fn destroys(kind: DestructiveKind) -> bool {
    !matches!(
        kind,
        DestructiveKind::ChangesRole
            | DestructiveKind::StopsService
            | DestructiveKind::RestartsService
            | DestructiveKind::UnprotectsEnvironment
    )
}

/// Unix seconds as RFC 3339 in `zone`, the way `ployz logs` prints its times.
fn moment<Zone: TimeZone>(at: u64, zone: &Zone) -> String
where
    Zone::Offset: Display,
{
    i64::try_from(at)
        .ok()
        .and_then(|seconds| zone.timestamp_opt(seconds, 0).single())
        .map_or_else(
            || at.to_string(),
            |time| time.to_rfc3339_opts(SecondsFormat::Secs, false),
        )
}

fn things(count: usize) -> String {
    format!("{count} thing{}", if count == 1 { "" } else { "s" })
}

pub(crate) fn line(effect: &DestructiveEffect) -> String {
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
        DestructiveKind::StopsService => format!("stops Service {name} until started again"),
        DestructiveKind::RestartsService => format!("restarts Service {name}"),
        DestructiveKind::UnprotectsEnvironment => format!("unprotects Environment {name}"),
        DestructiveKind::RemovesOrganization => format!("removes Organization {name}"),
        DestructiveKind::RestoresVolume => format!("restores Volume {name} to a snapshot"),
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
    if crate::ui::can_prompt() && !asked.for_someone_else() {
        crate::ui::confirm_name(
            asked.name(),
            || Failure::usage("Approving needs a terminal"),
            &format!(
                "Nothing {}; the approval stays pending in Ployz Cloud.",
                verb.past()
            ),
        )
        .map_err(StoreCallError::Stopped)?;
        let approve = serde_json::json!({ "approve": { "digest": asked.required.digest } });
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
    if asked.for_someone_else() {
        if !asked.reason.is_empty() {
            crate::ui::note(&asked.reason);
        }
        crate::ui::note(for_another_approver(credential, &asked.id));
        crate::ui::hint(&Hint::Inspect(format!(
            "ployz org approval show {}",
            asked.id
        )));
    } else {
        crate::ui::note(where_to_approve(
            credential,
            &asked.id,
            asked.required.organization.as_deref(),
        ));
        crate::ui::hint(&Hint::Next(format!(
            "ployz org approval approve {}",
            asked.id
        )));
    }
    crate::ui::note_inline("Waiting for approval in Ployz Cloud… ");
    let settled = runtime.block_on(async {
        tokio::select! {
            () = interrupted.cancelled() => None,
            settled = decided(credential, &asked.id) => Some(settled),
        }
    });
    crate::ui::note(match &settled {
        Some(Ok(Status::Approved)) => "approved.",
        Some(Ok(Status::Denied)) => "denied.",
        Some(Ok(Status::Superseded)) => "superseded; asking again.",
        Some(Ok(Status::Pending) | Err(_)) | None => "",
    });
    // Ctrl-C or losing Cloud ends the wait, not the approval: it is still there to answer.
    let stopped = match settled {
        Some(Ok(_)) => return Ok(asked.id.clone()),
        Some(Err(StoreCallError::Cloud(error))) if unanswered(&error) => Failure::from(error)
            .context(format!(
                "Stopped waiting for approval {}; it stays pending in Ployz Cloud.",
                asked.id
            )),
        // Cloud answered, but not with the approval: nothing says it is still pending.
        Some(Err(StoreCallError::Cloud(error))) => {
            let transient = matches!(error, LoginError::Status { status, .. } if status >= 500);
            let failure =
                Failure::from(error).context(format!("Couldn't check approval {}.", asked.id));
            if !transient {
                return Err(StoreCallError::Stopped(failure));
            }
            failure
        }
        Some(Err(error)) => return Err(error),
        None => Failure::coded(
            RpcErrorCode::Internal,
            "Stopped waiting; the approval stays pending in Ployz Cloud.",
        )
        .interrupted(),
    };
    Err(StoreCallError::Stopped(stopped.hint(Hint::Retry(retry))))
}

/// Where a human answers approval `id`: the Ployz agent's panel, where it waits under
/// Waiting on you, in the Organization the approval names, else the one this device acts
/// in. A token's approval that names none points at Cloud itself.
fn where_to_approve(credential: &Credential, id: &str, organization: Option<&str>) -> String {
    let cloud = credential.cloud().trim_end_matches('/');
    let organization = organization.or(match credential {
        Credential::Device(signed_in) => Some(signed_in.organization.slug.as_str()),
        Credential::Token { .. } => None,
    });
    match organization {
        Some(organization) => format!(
            "Approve {id} under Waiting on you in the Ployz agent at {cloud}/cloud/{organization}?chat={}",
            uuid::Uuid::new_v4()
        ),
        None => format!("Approve {id} in Ployz Cloud at {cloud}"),
    }
}

/// Who answers approval `id` when its asker can't, and where: Ployz Cloud, or the command
/// they run. The asker can only follow it, with `org approval show`.
fn for_another_approver(credential: &Credential, id: &str) -> String {
    let cloud = credential.cloud().trim_end_matches('/');
    format!(
        "An owner or admin who didn't ask must approve {id} in Ployz Cloud at {cloud}, or with ployz org approval approve {id}."
    )
}

/// Whether Cloud never answered the approval's read: only then is the approval as it was.
fn unanswered(error: &LoginError) -> bool {
    match error {
        LoginError::Unreachable { .. } | LoginError::Unanswered { .. } => true,
        // Cloud refused the credential, so nothing says the approval is still pending, and
        // a retry with that credential fails again. The local ones can't follow `settle()`
        // having a credential, but would mean the same.
        LoginError::Store { .. }
        | LoginError::Corrupt { .. }
        | LoginError::TokenCloudUnreadable(_)
        | LoginError::OtherCloud { .. }
        | LoginError::SignedOut
        | LoginError::Ended
        | LoginError::TokenRefused
        | LoginError::AgentTokenRefused
        | LoginError::TokenMalformed
        | LoginError::NotMember(_)
        | LoginError::Unsupported(_)
        | LoginError::Status { .. }
        | LoginError::Reply(_)
        | LoginError::Expired
        | LoginError::Denied
        | LoginError::AwaitingApproval { .. }
        | LoginError::TokenBound
        | LoginError::UnknownOrganization(_)
        | LoginError::UnknownCredential(_)
        | LoginError::AmbiguousCredential { .. } => false,
    }
}

/// Run `attempt` with `approval` until Cloud stops asking for one: each time it asks,
/// [`settle`] gets a human's answer and `attempt` runs again carrying it. Under `--json`
/// nothing asks; the refusal comes back naming `retry` of the approval's id.
pub(crate) fn approved<T>(
    runtime: &tokio::runtime::Runtime,
    credential: &Credential,
    verb: impl Into<Verb>,
    mut approval: Option<String>,
    retry: impl Fn(&str) -> String,
    mut attempt: impl AsyncFnMut(Option<&str>) -> Result<T, StoreCallError>,
) -> Result<T, Failure> {
    let verb = verb.into();
    loop {
        let refused = match runtime.block_on(attempt(approval.as_deref())) {
            Err(StoreCallError::Refused(refused)) => refused,
            other => return other.map_err(Failure::from),
        };
        let Some(asked) = Asked::of(&refused) else {
            return Err(refused.into());
        };
        if crate::ui::json() {
            return Err(Failure::from(refused).hint(Hint::Retry(retry(&asked.id))));
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

/// Wait for a human to answer the approval `id` in Ployz Cloud: its status once no longer pending.
pub(crate) async fn decided(credential: &Credential, id: &str) -> Result<Status, StoreCallError> {
    loop {
        let status = status(credential, id).await?;
        if status != Status::Pending {
            return Ok(status);
        }
        tokio::time::sleep(POLL).await;
    }
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
    use chrono::FixedOffset;

    use ployz_store::OperationVerb;

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
    fn an_approval_someone_else_answers_names_where_and_how() {
        let credential = crate::cloud_account::Credential::Token {
            cloud: "http://cloud.test/".into(),
            token: crate::cloud_login::Secret::new("ployz_secret".into()),
            from: crate::cloud_account::TokenFrom::Env,
        };
        assert_eq!(
            super::for_another_approver(&credential, "a1"),
            "An owner or admin who didn't ask must approve a1 in Ployz Cloud at http://cloud.test, or with ployz org approval approve a1."
        );
    }

    #[test]
    fn a_cluster_rename_reads_as_a_rename_to_its_new_name() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
            "approval_id": "a1", "approval": "d1", "effects": [],
            "operation": { "verb": "set_cluster_name", "name": "acme" },
        }))
        .unwrap();
        assert_eq!(asked.verb(), Verb::Operation(OperationVerb::SetClusterName));
        assert_eq!(asked.what(asked.verb()), "this rename to acme");
    }

    #[test]
    fn a_cluster_rename_shows_the_private_addresses_it_moves() {
        let review = |preview: serde_json::Value| {
            let asked: Asked = serde_json::from_value(serde_json::json!({
                "approval_id": "a1", "approval": "d1", "effects": [], "organization": "ada",
                "operation": { "verb": "set_cluster_name", "name": "beta", "preview": preview },
            }))
            .unwrap();
            asked.review_in(asked.verb(), &chrono::Utc)
        };
        assert_eq!(
            review(serde_json::json!({ "from": "alpha", "to": "beta" })),
            [
                "This rename in Organization ada:",
                "  ~ *.alpha.ployz.internal → *.beta.ployz.internal",
            ]
        );
        assert_eq!(
            review(serde_json::json!({ "from": null, "to": "beta" })),
            [
                "This rename in Organization ada:",
                "  ~ *.beta.ployz.internal"
            ]
        );
    }

    #[test]
    fn server_removal_approval_keeps_volume_data_on_disk() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
            "approval_id": "a1", "approval": "d1",
            "effects": [{ "kind": "deletes_volume", "name": "pgdata", "node": "v1", "path": "volumes/v1" }],
            "operation": { "subject": "server:box", "verb": "remove", "name": "box" },
        })).unwrap();
        let review = asked.review(asked.verb()).join("\n");
        assert!(review.contains("loses access to Volume pgdata"), "{review}");
        assert!(
            review.contains("its data stays on the Server's disk until wiped"),
            "{review}"
        );
        assert!(
            review.contains("Wipe or reinstall the disk to erase it"),
            "{review}"
        );
        assert!(
            review.contains("sudo ployz-uninstall keeps the data"),
            "{review}"
        );
        assert!(!review.contains("deletes Volume"), "{review}");
        assert!(!review.contains("destroys"), "{review}");
    }

    #[test]
    fn volume_deletion_approval_still_says_it_deletes_data() {
        for (subject, verb) in [
            (Some("project:shop"), "remove"),
            (Some("namespace:shop"), "clean"),
            (Some("server:box"), "clean"),
            (Some("server:box"), "forget"),
            (Some("servers:acme"), "forget"),
            (None, "remove"),
        ] {
            let mut operation = serde_json::json!({ "verb": verb, "name": "shop" });
            if let Some(subject) = subject {
                operation
                    .as_object_mut()
                    .unwrap()
                    .insert("subject".into(), subject.into());
            }
            let asked: Asked = serde_json::from_value(serde_json::json!({
                "approval_id": "a1", "approval": "d1",
                "effects": [{ "kind": "deletes_volume", "name": "pgdata", "node": "v1", "path": "volumes/v1" }],
                "operation": operation,
            })).unwrap();
            assert!(
                asked
                    .review(asked.verb())
                    .join("\n")
                    .contains("deletes Volume pgdata and its data")
            );
        }
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
    fn a_save_that_only_changes_roles_destroys_nothing() {
        let asked = asked("set", "changes_role");
        assert_eq!(
            asked.review(Verb::Publish),
            [
                "The next deploy in Organization acme after this save:",
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

    #[test]
    fn a_deploy_that_destroys_nothing_reads_as_the_deploy() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
            "approval_id": "a1",
            "approval": "d1",
            "effects": [],
            "diff": {
                "environment": {
                    "id": "00000000-0000-4000-8000-000000000002",
                    "project": "shop",
                    "name": "production",
                    "revision": 3,
                },
                "version": "3:2:0.1",
                "saved": 2,
                "holds_staged": false,
                "changes": [],
                "total_count": 2,
            },
        }))
        .unwrap();
        assert_eq!(
            asked.review(Verb::Deploy),
            ["This deploy:", "  + 2 other changes"]
        );
    }

    #[test]
    fn a_deploy_that_prunes_says_it_removes_any_other_service() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
            "approval_id": "a1",
            "approval": "d1",
            "effects": [],
            "prunes": true,
            "diff": {
                "environment": {
                    "id": "00000000-0000-4000-8000-000000000002",
                    "project": "shop",
                    "name": "production",
                    "revision": 3,
                },
                "version": "3:2:0.1",
                "saved": 2,
                "holds_staged": false,
                "changes": [],
                "total_count": 1,
            },
        }))
        .unwrap();
        assert_eq!(
            asked.review(Verb::Deploy),
            [
                "This deploy:",
                "  ✕ removes any other Service running in this environment",
                "  + 1 other change",
            ]
        );
    }

    #[test]
    fn a_prune_says_which_servers_drop_large_packets() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
            "approval_id": "a1",
            "approval": "d1",
            "effects": [],
            "prunes": true,
            "dropped": ["machine-3"],
            "diff": {
                "environment": {
                    "id": "00000000-0000-4000-8000-000000000002",
                    "project": "shop",
                    "name": "production",
                    "revision": 3,
                },
                "version": "3:2:0.1",
                "saved": 2,
                "holds_staged": false,
                "changes": [],
                "total_count": 0,
            },
        }))
        .unwrap();
        assert_eq!(
            asked.review(Verb::Deploy),
            [
                "This deploy:",
                "  ✕ removes any other Service running in this environment",
                "    couldn't check every server: large packets to machine-3 are dropped on the mesh path",
            ]
        );
    }

    #[test]
    fn a_restores_detach_says_which_changes_it_leaves_behind_in_the_given_zone() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
            "approval_id": "a1",
            "approval": "d1",
            "effects": [{
                "kind": "detaches_volume",
                "name": "pg-data",
                "node": "s1",
                "path": "postgres.mounts.pg-data",
                "lost_after": { "at": 1_791_451_620_u64, "restored": "pg-data-0927" },
            }],
            "diff": {
                "environment": {
                    "id": "00000000-0000-4000-8000-000000000002",
                    "project": "shop",
                    "name": "production",
                    "revision": 3,
                },
                "version": "3:2:0.1",
                "saved": 2,
                "holds_staged": false,
                "changes": [],
                "total_count": 1,
            },
        }))
        .unwrap();
        let berlin = FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(
            asked.review_in(Verb::Deploy, &berlin),
            [
                "This deploy destroys 1 thing:",
                "  ✕ detaches Volume pg-data from postgres",
                "    Changes after 2026-10-08T11:27:00+02:00 are not in pg-data-0927",
            ]
        );
    }

    /// A protected redeploy that destroys nothing still names what it asks about and
    /// why, rather than an empty `This deploy:` (#3417), and one that replays a
    /// teardown names what it removes (#3471).
    #[test]
    fn a_protected_redeploy_names_its_deployment_rollback_and_reason() {
        let refusal =
            |rolls_back: serde_json::Value, effects: serde_json::Value| -> ployz_core::RpcError {
                serde_json::from_value(serde_json::json!({
                    "code": "approval_required",
                    "message": "production is Protected; an admin must approve this deploy.",
                    "details": {
                        "approval_id": "a1",
                        "approval": "d1",
                        "effects": effects,
                        "diff": {
                            "environment": {
                                "id": "00000000-0000-4000-8000-000000000002",
                                "project": "shop",
                                "name": "production",
                                "revision": 3,
                            },
                            "version": "3:2:0.1",
                            "saved": 2,
                            "holds_staged": false,
                            "changes": [],
                            "total_count": 0,
                        },
                        "redeploy": { "deployment": 4, "rolls_back": rolls_back },
                        "protected": true,
                    },
                }))
                .unwrap()
            };
        let none = serde_json::json!([]);
        let asked = Asked::of(&refusal(serde_json::json!([]), none.clone())).unwrap();
        assert!(asked.for_someone_else());
        assert_eq!(asked.review(Verb::Deploy), ["This redeploy of #4:"]);
        assert_eq!(
            asked.reason,
            "production is Protected; an admin must approve this deploy."
        );

        let asked = Asked::of(&refusal(serde_json::json!([5, 6]), none)).unwrap();
        assert_eq!(
            asked.review(Verb::Deploy),
            [
                "This redeploy of #4:",
                "  ~ rolls back to #4, undoing #5 and #6"
            ]
        );

        let teardown = serde_json::json!([
            { "kind": "removes_service", "name": "extra", "node": "s1", "path": "extra" },
            { "kind": "removes_domain", "name": "app.example.com", "node": "s2", "path": "web.routes.r1" },
        ]);
        let asked = Asked::of(&refusal(serde_json::json!([]), teardown)).unwrap();
        assert_eq!(
            asked.review(Verb::Deploy),
            [
                "This redeploy of #4 destroys 2 things:",
                "  ✕ removes Service extra",
                "  ✕ removes domain app.example.com",
            ]
        );
    }

    #[test]
    fn a_protected_ask_waits_for_someone_else_even_at_a_terminal() {
        let ask = |protected: serde_json::Value| -> Asked {
            serde_json::from_value(serde_json::json!({
                "approval_id": "a1",
                "approval": "d1",
                "effects": [],
                "operation": { "verb": "move", "name": "pg-data" },
                "protected": protected,
            }))
            .unwrap()
        };
        assert!(ask(serde_json::json!({ "environment": "production" })).for_someone_else());
        assert!(ask(serde_json::json!(true)).for_someone_else());
        assert!(!ask(serde_json::Value::Null).for_someone_else());
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
                serde_json::from_value::<OperationVerb>(serde_json::json!(verb)).is_ok(),
                "Cloud asks approval to {verb}, which `OperationVerb` does not read"
            );
        }
    }

    /// The words each verb Cloud names puts on an approval card are frozen with its text.
    #[test]
    fn every_verb_keeps_its_text_and_its_words() {
        for (verb, noun, past, preposition) in [
            ("drain", "drain", "drained", "of"),
            ("set", "change", "changed", "to"),
            ("clean", "cleanup", "cleaned", "of"),
            ("remove", "removal", "removed", "of"),
            ("forget", "forgetting", "forgotten", "of"),
            ("release", "release", "released", "of"),
            ("mirror", "mirror", "mirrored", "of"),
            ("sync", "full sync", "synced", "of"),
            ("remove_mirror", "mirror removal", "removed", "of"),
            ("stop", "stop", "stopped", "of"),
            ("restart", "restart", "restarted", "of"),
            ("unprotect", "unprotecting", "unprotected", "of"),
            ("move", "move", "moved", "of"),
            ("restore", "restore", "restored", "of"),
            ("resize", "resize", "resized", "of"),
            ("set_cluster_name", "rename", "renamed", "to"),
            ("set_ingress_doors", "door change", "changed", "to"),
            (
                "set_private_access",
                "Private Access change",
                "changed",
                "to",
            ),
        ] {
            let asked = asked(verb, "removes_member");
            assert_eq!(asked.verb().noun(), noun, "{verb}");
            assert_eq!(asked.verb().past(), past, "{verb}");
            assert_eq!(
                asked.what(asked.verb()),
                format!("this {noun} {preposition} bob@example.com"),
                "{verb}"
            );
        }
        for (verb, noun, past) in [
            (Verb::Publish, "save", "saved"),
            (Verb::Deploy, "deploy", "deployed"),
            (Verb::Retry, "redeploy", "retried"),
        ] {
            assert_eq!((verb.noun(), verb.past()), (noun, past));
        }
    }

    /// A verb this CLI cannot read leaves the refusal unasked, whatever else it names.
    #[test]
    fn a_verb_nobody_taught_this_cli_leaves_the_refusal_unasked() {
        let refusal = |verb: &str| -> ployz_core::RpcError {
            serde_json::from_value(serde_json::json!({
                "code": "approval_required",
                "message": "A human must approve this.",
                "details": {
                    "approval_id": "a1", "approval": "d1", "effects": [],
                    "operation": { "verb": verb, "name": "web" },
                },
            }))
            .unwrap()
        };
        assert!(Asked::of(&refusal("restart")).is_some());
        for verb in ["frobnicate", "Restart", "set-cluster-name", ""] {
            assert!(Asked::of(&refusal(verb)).is_none(), "{verb}");
        }
    }

    /// Every key Cloud and the Store send on an approval card, read as one ask.
    #[test]
    fn an_ask_with_every_key_reads_as_it_always_did() {
        let operation: ployz_core::RpcError = serde_json::from_value(serde_json::json!({
            "code": "approval_required",
            "message": "A human must approve moving pg-data before it runs.",
            "details": {
                "approval_id": "a1", "approval": "d1",
                "effects": [{ "kind": "deletes_volume", "name": "pg-data", "node": "v1", "path": "volumes/v1" }],
                "operation": { "verb": "remove", "name": "box", "subject": "server:box", "preview": { "from": "alpha" } },
                "organization": "ada",
                "protected": { "environment": "production" },
            },
        }))
        .unwrap();
        let asked = Asked::of(&operation).unwrap();
        assert_eq!(
            (
                asked.id.as_str(),
                asked.required.digest.as_str(),
                asked.name()
            ),
            ("a1", "d1", "box")
        );
        assert!(asked.for_someone_else());
        assert_eq!(
            asked.reason,
            "A human must approve moving pg-data before it runs."
        );
        assert_eq!(
            asked.review_in(asked.verb(), &chrono::Utc),
            [
                "This removal in Organization ada:",
                "  ✕ loses access to Volume pg-data; its data stays on the Server's disk until wiped. Wipe or reinstall the disk to erase it; sudo ployz-uninstall keeps the data.",
            ]
        );

        let diff: ployz_core::RpcError = serde_json::from_value(serde_json::json!({
            "code": "approval_required",
            "message": "m",
            "details": {
                "approval_id": "a2", "approval": "d2",
                "effects": [{ "kind": "removes_service", "name": "web", "node": "s1", "path": "web" }],
                "diff": {
                    "environment": { "id": "00000000-0000-4000-8000-000000000002", "project": "shop", "name": "production", "revision": 3 },
                    "version": "3:2:0.1", "saved": 2, "holds_staged": false, "changes": [], "total_count": 3,
                },
                "prunes": true,
                "dropped": ["box-1", "box-2"],
                "redeploy": { "deployment": 4, "rolls_back": [5] },
            },
        }))
        .unwrap();
        let asked = Asked::of(&diff).unwrap();
        assert!(!asked.for_someone_else());
        assert_eq!(asked.name(), "production");
        assert_eq!(asked.what(Verb::Deploy), "this deploy to shop/production");
        assert_eq!(
            asked.review_in(Verb::Deploy, &chrono::Utc),
            [
                "This redeploy of #4 destroys 1 thing:",
                "  ✕ removes Service web",
                "  ~ rolls back to #4, undoing #5",
                "  ✕ removes any other Service running in this environment",
                "    couldn't check every server: large packets to box-1, box-2 are dropped on the mesh path",
                "  + 2 other changes",
            ]
        );
    }
}
