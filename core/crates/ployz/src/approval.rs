//! A person's yes to a write or an operation that destroys something. Cloud refuses it
//! with `approval_required` and the digest of what it would do; the CLI shows that, asks
//! yes or no, and on a yes sends the request again carrying the digest. Cloud keeps
//! nothing: it runs the request only while its plan still has that digest.

use std::fmt::Display;

use chrono::{Local, SecondsFormat, TimeZone};
use ployz_core::RpcError;
use ployz_core::domain::private_dns;
use ployz_store::{
    ApprovalOperation, ApprovalRequired, ApprovalSubject, DestructiveEffect, DestructiveKind,
    DiffView, OperationVerb, SettingPath,
};
use serde::Deserialize;

use crate::cli::Rerun;
use crate::cloud::account::StoreCallError;
use crate::failure::Failure;

/// What the approval covers: a Store write, which carries its diff, or an operation on one
/// named thing.
pub(crate) type Subject = ApprovalSubject<Box<DiffView>>;

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub(crate) struct Asked {
    pub(crate) required: ApprovalRequired<Box<DiffView>>,
}

fn renamed_addresses(operation: &ApprovalOperation) -> String {
    let to = private_dns::alias_wildcard(&operation.name);
    match operation.renamed_from() {
        Some(from) => format!("{} → {to}", private_dns::alias_wildcard(from)),
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
    Cancel,
    Sync,
    Take,
    ShutDown,
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
            Self::Cancel => "cancel",
            Self::Sync => "sync",
            Self::Take => "take",
            Self::ShutDown => "shutdown",
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
            Self::Cancel => "cancelled",
            Self::Sync => "synced",
            Self::Take => "taken",
            Self::ShutDown => "shut down",
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
            | Self::Cancel
            | Self::Sync
            | Self::Take
            | Self::ShutDown
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

    /// `this deploy to shop/production`, but `this sync into shop/fix`.
    fn toward(self) -> &'static str {
        match self {
            Self::Sync | Self::Take => "into",
            Self::Cancel => "in",
            Self::ShutDown => "of",
            Self::Publish | Self::Deploy | Self::Retry | Self::Operation(_) => "to",
        }
    }
}

impl Asked {
    pub(crate) fn of(error: &RpcError) -> Option<Self> {
        if error.code.as_str() != ployz_core::APPROVAL_REQUIRED_CODE {
            return None;
        }
        serde_json::from_value(error.details.clone()).ok()
    }

    /// The digest a yes covers, which the request carries when it is sent again.
    pub(crate) fn digest(&self) -> &str {
        &self.required.digest
    }

    /// The verb Cloud named. A diff names none, so `command`, the Store write that
    /// waits, says what it is; without one it is a deploy.
    pub(crate) fn verb(&self, command: Option<&ployz_store::Command>) -> Verb {
        use ployz_store::{Admit, Command};
        match (&self.required.subject, command) {
            (Subject::Operation(operation), _) => Verb::Operation(operation.verb),
            (Subject::Diff(_), Some(Command::Publish(_))) => Verb::Publish,
            (Subject::Diff(_), Some(Command::Admit(Admit::Retry(_)))) => Verb::Retry,
            (Subject::Diff(_), Some(Command::Admit(Admit::ShutDown(_)))) => Verb::ShutDown,
            (Subject::Diff(_), Some(Command::Cancel(_))) => Verb::Cancel,
            (Subject::Diff(_), Some(Command::Sync(_))) => Verb::Sync,
            (Subject::Diff(_), Some(Command::Take(_))) => Verb::Take,
            (Subject::Diff(_), Some(Command::Remove(_) | Command::Admit(Admit::Remove(_)))) => {
                OperationVerb::Remove.into()
            }
            (Subject::Diff(_), Some(_) | None) => Verb::Deploy,
        }
    }

    /// `this deploy to shop/production`, `this removal of web-1`, `this change to web-2`.
    pub(crate) fn what(&self, verb: Verb) -> String {
        let noun = verb.noun();
        match &self.required.subject {
            Subject::Diff(diff) => format!(
                "this {noun} {} {}/{}",
                verb.toward(),
                diff.environment.project,
                diff.environment.name
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
        } else if changes_only {
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
            lines.push(format!("  {mark} {}", line(effect)));
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
        let region = match &self.required.subject {
            Subject::Diff(diff) => diff.region.as_ref(),
            Subject::Operation(_) => None,
        };
        if let Some(region) = region {
            let [before, after] = region.sides();
            lines.push(format!("  ~ {}: {before} → {after}", SettingPath::REGION));
        }
        let others = match &self.required.subject {
            Subject::Diff(diff) => diff
                .total_count
                .saturating_sub(self.required.effects.len() + usize::from(region.is_some())),
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

/// Shows what `asked` would do and asks yes or no: the digest to send again on a yes.
/// Without a terminal nothing asks, and the failure names `retry`.
pub(crate) fn settle(verb: Verb, asked: &Asked, retry: Rerun) -> Result<String, StoreCallError> {
    for line in asked.review(verb) {
        crate::ui::note(line);
    }
    crate::ui::confirm(
        &format!("Go ahead with {}?", asked.what(verb)),
        || {
            Failure::usage(format!(
                "Nothing {}. Saying yes needs a terminal.",
                verb.past()
            ))
            .hint(retry)
        },
        &format!("Nothing {}.", verb.past()),
    )
    .map_err(StoreCallError::Stopped)?;
    Ok(asked.digest().to_owned())
}

/// Cloud asked again after a yes: what it would do moved since it was shown.
pub(crate) fn moved(verb: Verb, refused: RpcError) -> Failure {
    Failure::from(refused).context(format!(
        "It changed after your yes, so nothing {}. Run it again to see what it does now.",
        verb.past()
    ))
}

/// Run `attempt` with `approval`, the digest a person already said yes to, if any. When
/// Cloud asks, [`settle`] gets a yes and `attempt` runs once more carrying its digest. An
/// ask after a yes means the plan moved, and nothing runs. Under `--json` nothing asks;
/// the refusal comes back naming `again` carrying the digest.
pub(crate) fn approved<T>(
    runtime: &tokio::runtime::Runtime,
    verb: impl Into<Verb>,
    mut approval: Option<String>,
    again: &Rerun,
    mut attempt: impl AsyncFnMut(Option<&str>) -> Result<T, StoreCallError>,
) -> Result<T, Failure> {
    let verb = verb.into();
    let mut said_yes = approval.is_some();
    loop {
        let refused = match runtime.block_on(attempt(approval.as_deref())) {
            Err(StoreCallError::Refused(refused)) => refused,
            other => return other.map_err(Failure::from),
        };
        let Some(asked) = Asked::of(&refused) else {
            return Err(refused.into());
        };
        if crate::ui::json() {
            return Err(Failure::from(refused).hint(again.clone().set("approval", asked.digest())));
        }
        if said_yes {
            return Err(moved(verb, refused));
        }
        approval = Some(settle(
            verb,
            &asked,
            again.clone().set("approval", asked.digest()),
        )?);
        said_yes = true;
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use ployz_store::OperationVerb;

    use super::{Asked, Verb};

    fn asked(verb: &str, kind: &str) -> Asked {
        serde_json::from_value(serde_json::json!({
            "approval": "d1",
            "effects": [{ "kind": kind, "name": "bob@example.com", "node": "u1", "path": "members/u1" }],
            "operation": { "verb": verb, "name": "bob@example.com" },
            "organization": "acme",
        }))
        .unwrap()
    }

    #[test]
    fn a_cluster_rename_reads_as_a_rename_to_its_new_name() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
            "approval": "d1", "effects": [],
            "operation": { "verb": "set_cluster_name", "name": "acme" },
        }))
        .unwrap();
        assert_eq!(
            asked.verb(None),
            Verb::Operation(OperationVerb::SetClusterName)
        );
        assert_eq!(asked.what(asked.verb(None)), "this rename to acme");
    }

    #[test]
    fn a_cluster_rename_shows_the_private_addresses_it_moves() {
        let review = |preview: serde_json::Value| {
            let asked: Asked = serde_json::from_value(serde_json::json!({
                "approval": "d1", "effects": [], "organization": "ada",
                "operation": { "verb": "set_cluster_name", "name": "beta", "preview": preview },
            }))
            .unwrap();
            asked.review_in(asked.verb(None), &chrono::Utc)
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
    fn volume_deletion_approval_still_says_it_deletes_data() {
        for (subject, verb) in [
            (Some("project:shop"), "remove"),
            (Some("namespace:shop"), "clean"),
            (Some("server:box"), "clean"),
            (Some("server:box"), "remove"),
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
                "approval": "d1",
                "effects": [{ "kind": "deletes_volume", "name": "pgdata", "node": "v1", "path": "volumes/v1" }],
                "operation": operation,
            })).unwrap();
            assert!(
                asked
                    .review(asked.verb(None))
                    .join("\n")
                    .contains("deletes Volume pgdata and its data")
            );
        }
    }

    #[test]
    fn a_role_change_reads_as_a_change_in_its_organization() {
        let asked = asked("set", "changes_role");
        assert_eq!(
            asked.review(asked.verb(None)),
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
            asked.review(asked.verb(None)),
            [
                "This removal in Organization acme destroys 1 thing:",
                "  ✕ removes member bob@example.com",
            ]
        );
    }

    #[test]
    fn a_deploy_that_destroys_nothing_reads_as_the_deploy() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
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
    fn a_cancel_sync_take_or_shutdown_ask_names_itself_not_a_deploy() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
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
                "total_count": 0,
            },
        }))
        .unwrap();
        let production = serde_json::json!({ "environment": "production" });
        for (command, what, past) in [
            (
                serde_json::json!({
                    "command": "cancel",
                    "deployment": "00000000-0000-4000-8000-000000000009",
                }),
                "this cancel in shop/production",
                "cancelled",
            ),
            (
                serde_json::json!({
                    "command": "sync", "from": { "environment": "fix" }, "into": production,
                    "version": "1", "close_after": false,
                }),
                "this sync into shop/production",
                "synced",
            ),
            (
                serde_json::json!({ "command": "take", "from": "main", "version": "1" }),
                "this take into shop/production",
                "taken",
            ),
            (
                serde_json::json!({
                    "command": "admit", "admit": "shut_down",
                    "id": "00000000-0000-4000-8000-000000000009", "environment": production,
                }),
                "this shutdown of shop/production",
                "shut down",
            ),
        ] {
            let command = ployz_store::Command::decode(command).unwrap();
            let verb = asked.verb(Some(&command));
            assert_eq!(asked.what(verb), what);
            assert_eq!(verb.past(), past);
        }
    }

    #[test]
    fn a_deploy_that_changes_the_region_says_where_the_services_run_next() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
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
                "region": { "before": null, "after": "fsn" },
                "total_count": 2,
            },
        }))
        .unwrap();
        assert_eq!(
            asked.review(Verb::Deploy),
            [
                "This deploy:",
                "  ~ @environment.region: any region → fsn",
                "  + 1 other change",
            ]
        );
    }

    #[test]
    fn a_deploy_that_prunes_says_it_removes_any_other_service() {
        let asked: Asked = serde_json::from_value(serde_json::json!({
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
    fn a_redeploy_names_its_deployment_and_what_it_rolls_back() {
        let refusal =
            |rolls_back: serde_json::Value, effects: serde_json::Value| -> ployz_core::RpcError {
                serde_json::from_value(serde_json::json!({
                    "code": "approval_required",
                    "message": "production is Protected; an admin must approve this deploy.",
                    "details": {
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
        assert_eq!(asked.review(Verb::Deploy), ["This redeploy of #4:"]);

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

    /// Cloud names every operation it gates with one of its `OperationVerb`s; one this CLI
    /// cannot read leaves its `approval_required` refusal unasked.
    #[test]
    fn every_operation_verb_cloud_sends_is_a_verb() {
        let approvals = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../dashboard/src/modules/operations/asked.ts");
        let source = std::fs::read_to_string(approvals).unwrap();
        let union = source
            .lines()
            .find_map(|line| line.strip_prefix("export type OperationVerb = "))
            .expect(
                "wants `export type OperationVerb = ...` on one line in dashboard/src/modules/operations/asked.ts",
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

    /// A verb this CLI cannot read leaves the refusal unasked, whatever else it names.
    #[test]
    fn a_verb_nobody_taught_this_cli_leaves_the_refusal_unasked() {
        let refusal = |verb: &str| -> ployz_core::RpcError {
            serde_json::from_value(serde_json::json!({
                "code": "approval_required",
                "message": "A human must approve this.",
                "details": {
                    "approval": "d1", "effects": [],
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
                "approval": "d1",
                "effects": [{ "kind": "deletes_volume", "name": "pg-data", "node": "v1", "path": "volumes/v1" }],
                "operation": { "verb": "remove", "name": "box", "subject": "server:box", "preview": { "from": "alpha" } },
                "organization": "ada",
                "protected": { "environment": "production" },
            },
        }))
        .unwrap();
        let asked = Asked::of(&operation).unwrap();
        assert_eq!(asked.digest(), "d1");
        assert_eq!(
            asked.review_in(asked.verb(None), &chrono::Utc),
            [
                "This removal in Organization ada destroys 1 thing:",
                "  ✕ deletes Volume pg-data and its data",
            ]
        );

        let diff: ployz_core::RpcError = serde_json::from_value(serde_json::json!({
            "code": "approval_required",
            "message": "m",
            "details": {
                "approval": "d2",
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
