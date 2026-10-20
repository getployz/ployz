//! Approval: what a review destroys, as Destructive Effects, and the gate that
//! refuses until a human approved exactly that set. Cloud says whether a human must
//! approve; the Store never takes the caller's word for it.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent, SavedVolumeIntent};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::id::{ApprovalDigest, EnvironmentId, Hostname, VolumeName};
use crate::review::{DiffView, Head, Review, Shipping};
use crate::scope::{Environment, EnvironmentSummary};
use crate::storage::Tx;
use crate::{Actor, Approval, Asks, Permission, Trusted};

/// One thing a publication destroys, by the node it changes and the row.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
pub struct DestructiveEffect {
    pub kind: DestructiveKind,
    /// What a human calls the thing destroyed: the Service, the Volume, or the
    /// domain's hostname.
    #[serde(default)]
    #[ts(as = "Option<String>", optional)]
    pub name: String,
    /// The node's ID: a Service's, or a Volume's resource ID.
    pub node: String,
    /// The row it changes, as `diff` shows it.
    pub path: String,
    /// A detach that moves the Service onto a Volume restored from the detached
    /// one: the writes it leaves behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lost_after: Option<LostAfter>,
}

/// What a restore leaves behind: the source's writes after `at` are not in
/// `restored`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
pub struct LostAfter {
    /// The restore point's time, in Unix seconds.
    #[ts(type = "number")]
    pub at: u64,
    /// The restored Volume's name.
    pub restored: String,
}

/// What a Destructive Effect destroys.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DestructiveKind {
    /// A deployed Service is removed.
    RemovesService,
    /// A deployed Volume is deleted, with its data.
    DeletesVolume,
    /// A deployed Service stops mounting a Volume.
    DetachesVolume,
    /// A deployed Service stops serving a domain.
    RemovesDomain,
    /// A Server leaves the cluster.
    RemovesServer,
    /// A member leaves the Organization, with their tokens and their devices' access.
    RemovesMember,
    /// A member's role in the Organization changes.
    ChangesRole,
    /// A pending invite's link stops working.
    RevokesInvite,
    /// A running Service's containers stop until started again.
    StopsService,
    /// A running Service's containers restart, dropping requests in flight.
    RestartsService,
    /// A Protected Environment can be removed, and anyone who can stage can change its source.
    UnprotectsEnvironment,
    /// An Organization is deleted, with its tokens, settings and Servers' pairing.
    RemovesOrganization,
}

impl DestructiveEffect {
    pub(crate) fn describe(&self) -> String {
        let name = &self.name;
        match self.kind {
            DestructiveKind::RemovesService => format!("remove Service {name}"),
            DestructiveKind::DeletesVolume => format!("delete Volume {name}"),
            DestructiveKind::DetachesVolume => match self.path.split_once(".mounts.") {
                Some((service, _)) => format!("detach Volume {name} from {service}"),
                None => format!("detach Volume {name}"),
            },
            DestructiveKind::RemovesDomain => format!("remove domain {name}"),
            DestructiveKind::RemovesServer => format!("remove Server {name}"),
            DestructiveKind::RemovesMember => format!("remove member {name}"),
            DestructiveKind::ChangesRole => format!("change the role of {name}"),
            DestructiveKind::RevokesInvite => format!("revoke the invite for {name}"),
            DestructiveKind::StopsService => format!("stop Service {name}"),
            DestructiveKind::RestartsService => format!("restart Service {name}"),
            DestructiveKind::UnprotectsEnvironment => format!("unprotect Environment {name}"),
            DestructiveKind::RemovesOrganization => format!("remove Organization {name}"),
        }
    }
}

/// What runs, or will once the Deployments in flight apply: Applied State plus each
/// node, route, generated domain and mount one of them puts in place, newest first.
/// Applied State names what it holds; a Volume no Service mounts comes in only with
/// Applied State, since a Deploy creates storage only with a mount.
pub(crate) fn deployed(head: &Head) -> SavedEnvironmentIntent {
    let mut deployed = head.applied.clone();
    for target in &head.in_flight {
        let mounted = target.volumes.iter().filter(|volume| {
            target.services.iter().any(|service| {
                service
                    .volume_attachments
                    .iter()
                    .any(|mount| mount.volume_resource_id == volume.resource_id)
            })
        });
        add_missing(&mut deployed.volumes, mounted, |volume| &volume.resource_id);
        for service in &target.services {
            let Some(running) = deployed
                .services
                .iter_mut()
                .find(|running| running.id == service.id)
            else {
                deployed.services.push(service.clone());
                continue;
            };
            let (into, from) = (&mut running.config, &service.config);
            add_missing(&mut into.routes, &from.routes, |route| &route.hostname);
            add_missing(
                &mut into.managed_hostnames,
                &from.managed_hostnames,
                |managed| &managed.prefix,
            );
            add_missing(
                &mut running.volume_attachments,
                &service.volume_attachments,
                |attachment| &attachment.volume_resource_id,
            );
        }
    }
    deployed
}

fn add_missing<'a, T: Clone + 'a, K: PartialEq + ?Sized>(
    into: &mut Vec<T>,
    from: impl IntoIterator<Item = &'a T>,
    key: impl Fn(&T) -> &K,
) {
    for item in from {
        if into.iter().all(|held| key(held) != key(item)) {
            into.push(item.clone());
        }
    }
}

/// The Volumes Service `id` mounts in `running` that `target` keeps, by identity,
/// and the Service no longer mounts there, removed or not.
pub(crate) fn detached<'a>(
    running: &'a SavedEnvironmentIntent,
    target: &'a SavedEnvironmentIntent,
    id: &'a str,
) -> impl Iterator<Item = &'a SavedVolumeIntent> {
    let mounts = move |intent: &'a SavedEnvironmentIntent| {
        intent
            .services
            .iter()
            .filter(move |service| service.id == id)
            .flat_map(|service| &service.volume_attachments)
    };
    mounts(running)
        .filter(move |mount| {
            mounts(target).all(|kept| kept.volume_resource_id != mount.volume_resource_id)
        })
        .filter_map(move |mount| {
            target
                .volumes
                .iter()
                .find(|volume| volume.resource_id == mount.volume_resource_id)
        })
}

/// What publishing `target` destroys of what is `running`, naming each generated domain
/// under its Cluster Domain in `cluster_domains`, by node ID and prefix.
pub(crate) fn destructive_effects(
    running: &SavedEnvironmentIntent,
    target: &SavedEnvironmentIntent,
    cluster_domains: &BTreeMap<(String, String), Hostname>,
) -> BTreeSet<DestructiveEffect> {
    let mut effects = BTreeSet::new();
    for volume in &running.volumes {
        if target
            .volumes
            .iter()
            .all(|kept| kept.resource_id != volume.resource_id)
        {
            effects.insert(DestructiveEffect {
                kind: DestructiveKind::DeletesVolume,
                name: volume.name.clone(),
                node: volume.resource_id.clone(),
                path: format!("volumes.{}", volume.name),
                lost_after: None,
            });
        }
    }
    for deployed in &running.services {
        let effect = |kind, name: &str, path: String| DestructiveEffect {
            kind,
            name: name.to_owned(),
            node: deployed.id.clone(),
            path,
            lost_after: None,
        };
        let Some(service) = target
            .services
            .iter()
            .find(|service| service.id == deployed.id)
        else {
            let slug = &deployed.slug;
            effects.insert(effect(DestructiveKind::RemovesService, slug, slug.clone()));
            continue;
        };
        let slug = &service.slug;
        for volume in detached(running, target, &deployed.id) {
            let path = format!("{slug}.mounts.{}", volume.name);
            effects.insert(DestructiveEffect {
                lost_after: lost_after(running, target, service, &volume.resource_id),
                ..effect(DestructiveKind::DetachesVolume, &volume.name, path)
            });
        }
        let config = &service.config;
        for route in &deployed.config.routes {
            if config
                .routes
                .iter()
                .all(|kept| kept.hostname != route.hostname)
            {
                let path = format!("{slug}.routes.{}", route.id);
                effects.insert(effect(
                    DestructiveKind::RemovesDomain,
                    &route.hostname,
                    path,
                ));
            }
        }
        for managed in &deployed.config.managed_hostnames {
            if config
                .managed_hostnames
                .iter()
                .all(|kept| kept.prefix != managed.prefix)
            {
                let prefix = &managed.prefix;
                let hostname = cluster_domains
                    .get(&(deployed.id.clone(), prefix.clone()))
                    .map_or_else(|| prefix.clone(), |cluster| format!("{prefix}.{cluster}"));
                let path = format!("{slug}.managedHostnames");
                effects.insert(effect(DestructiveKind::RemovesDomain, &hostname, path));
            }
        }
    }
    effects
}

/// What `service` loses by leaving Volume `source` for a Volume this publication
/// restores from it.
fn lost_after(
    running: &SavedEnvironmentIntent,
    target: &SavedEnvironmentIntent,
    service: &SavedServiceIntent,
    source: &str,
) -> Option<LostAfter> {
    target.volumes.iter().find_map(|restored| {
        let from = restored.restored_from.as_ref()?;
        let new = running
            .volumes
            .iter()
            .all(|held| held.resource_id != restored.resource_id);
        let mounts = service
            .volume_attachments
            .iter()
            .any(|mount| mount.volume_resource_id == restored.resource_id);
        (from.volume == source && new && mounts).then(|| LostAfter {
            at: from.at,
            restored: restored.name.clone(),
        })
    })
}

pub(crate) fn approval_digest(
    who: &Actor,
    environment: &EnvironmentId,
    version: &str,
    effects: &BTreeSet<DestructiveEffect>,
) -> ApprovalDigest {
    digest(version, &bound(who, environment, effects))
}

fn bound(
    who: &Actor,
    environment: &EnvironmentId,
    effects: &BTreeSet<DestructiveEffect>,
) -> serde_json::Value {
    json!({
        "organization": who.organization,
        "environment": environment,
        "effects": effects,
    })
}

fn digest(version: &str, bound: &serde_json::Value) -> ApprovalDigest {
    let digest = crate::removal::short_digest(&bound.to_string());
    ApprovalDigest::parse(format!("{version}:{digest}"))
        .expect("a version and a 16-digit hex digest form an approval")
}

/// What a write into one Environment must clear. Cloud says who asks; in a Protected
/// Environment a caller without `environments:protect` asks for every write that
/// ships, and Cloud's word that nothing is required does not exempt it. Cloud's own
/// writes name no caller, so they never ask here.
pub(crate) struct Gate<'a> {
    trusted: &'a Trusted,
    protected: bool,
}

impl<'a> Gate<'a> {
    pub(crate) fn new(
        tx: &mut dyn Tx,
        environment: &EnvironmentId,
        trusted: &'a Trusted,
    ) -> Result<Self, RpcError> {
        let protected = trusted
            .permissions
            .as_ref()
            .is_some_and(|held| !held.contains(&Permission::EnvironmentsProtect))
            && crate::scope::protected(tx, environment)?;
        Ok(Self { trusted, protected })
    }

    /// Whether a retry asks a human at all.
    pub(crate) fn asks_retry(&self) -> bool {
        self.protected
            || (self.trusted.asks == Asks::EveryDeploy
                && self.trusted.approval != Approval::NotRequired)
    }

    /// Refuse unless Cloud approved `bound` at `diff`'s version; a Protected ask binds
    /// that too, so an approval given before protection doesn't carry over.
    fn require(
        &self,
        mut bound: serde_json::Value,
        message: String,
        (effects, diff): (&BTreeSet<DestructiveEffect>, &DiffView),
    ) -> Result<(), RpcError> {
        if self.protected
            && let Some(bound) = bound.as_object_mut()
        {
            bound.insert("protected".to_owned(), json!(true));
        }
        let digest = digest(&diff.version, &bound);
        match &self.trusted.approval {
            Approval::NotRequired if !self.protected => Ok(()),
            Approval::Approved(approved) if *approved == digest => Ok(()),
            Approval::NotRequired | Approval::Required | Approval::Approved(_) => {
                let mut details = json!({"effects": effects, "approval": digest, "diff": diff});
                if self.protected
                    && let Some(details) = details.as_object_mut()
                {
                    details.insert("protected".to_owned(), json!(true));
                }
                Err(error::approval_required(message, details))
            }
        }
    }

    fn message(&self, diff: &DiffView, what: &str, otherwise: impl FnOnce() -> String) -> String {
        if self.protected {
            let name = &diff.environment.name;
            format!("{name} is Protected; an admin must approve this {what}.")
        } else {
            otherwise()
        }
    }
}

/// Refuse shipping `target` while it destroys something, unless no approval is
/// required or Cloud holds a human's approval of exactly this version and set.
/// Publishing already-Saved State asks nothing: the Publish that saved it did. Under
/// [`Asks::EveryDeploy`], or into a Protected Environment for a caller who asks
/// there, every Deploy and Publish asks, even one that destroys nothing or ships
/// already-Saved State, and a Deploy's approval covers only the Services and upload
/// it deploys.
///
/// # Errors
/// `approval_required`, naming the effects, the `version:digest` to approve and the
/// fresh `diff`, and `protected` when the Environment's protection asked.
pub(crate) fn approve(
    who: &Actor,
    review: &Review,
    (target, shipping): (&SavedEnvironmentIntent, Shipping<'_>),
    gate: &Gate<'_>,
) -> Result<(), RpcError> {
    let diff = &review.view;
    let environment = &diff.environment.id;
    let effects = &diff.effects;
    let resaves = review
        .saved
        .as_ref()
        .is_some_and(|saved| saved.intent == *target);
    let asks = match shipping {
        Shipping::Publish if gate.protected && !resaves => Asks::EveryDeploy,
        Shipping::Deploy { .. } if gate.protected => Asks::EveryDeploy,
        Shipping::Publish => Asks::Destroys,
        Shipping::Deploy { .. } => gate.trusted.asks,
    };
    if asks == Asks::Destroys && (effects.is_empty() || resaves) {
        return Ok(());
    }
    if !effects.is_empty() {
        gate.trusted.admit(Permission::ChangesDestroy)?;
    }
    let mut bound = bound(who, environment, effects);
    if let Shipping::Deploy { services, upload } = shipping
        && let Some(bound) = bound.as_object_mut()
    {
        let services = services.iter().collect::<BTreeSet<_>>();
        bound.insert("services".to_owned(), json!(services));
        bound.insert("upload".to_owned(), json!(upload));
    }
    let message = gate.message(diff, "deploy", || {
        if effects.is_empty() {
            "A human must approve this deploy first.".to_owned()
        } else {
            let asked = effects
                .iter()
                .map(DestructiveEffect::describe)
                .collect::<Vec<_>>()
                .join(", ");
            format!("A human must approve this first: {asked}")
        }
    });
    gate.require(bound, message, (effects, diff))
}

/// Refuse retrying Deployment `source` unless Cloud holds a human's approval of this
/// retry, when [`Gate::asks_retry`]: a retry ships nothing a review hasn't.
/// `review` is taken before the retry queues, so its version moves once it does: one
/// approval admits one retry. The approval binds the Deployments a rollback undoes,
/// the Volumes it removes and the losses the caller accepted, and says so.
///
/// # Errors
/// `approval_required`, naming the `version:digest` to approve, the `diff` and the
/// `redeploy`: its Deployment's number and what it rolls back.
pub(crate) fn approve_retry(
    who: &Actor,
    review: &Review,
    (retry, rollback): (&crate::Retry, &crate::deployment::Rollback),
    gate: &Gate<'_>,
) -> Result<(), RpcError> {
    // The Volumes a rollback removes, as a Deploy's card shows them.
    let effects: BTreeSet<DestructiveEffect> = rollback
        .removes
        .iter()
        .map(|loss| DestructiveEffect {
            kind: DestructiveKind::DeletesVolume,
            name: loss.volume.name.to_string(),
            node: loss.volume.id.to_string(),
            path: format!("volumes.{}", loss.volume.name),
            lost_after: None,
        })
        .collect();
    let accepted: BTreeSet<&VolumeName> = retry.accept_volume_loss.iter().collect();
    let bound = json!({
        "organization": who.organization,
        "environment": review.view.environment.id,
        "retry": retry.deployment,
        "number": rollback.number,
        "rolls_back": rollback.rolls_back,
        "effects": effects,
        "accept_volume_loss": accepted,
    });
    // The retry ships its source's frozen state: none of the staged changes, and it
    // destroys only what its source's review or the caller's acceptance covered.
    let mut diff = review.view.clone();
    diff.effects.clear();
    diff.changes.clear();
    diff.total_count = 0;
    let undoes = crate::deployment::numbered(&rollback.rolls_back);
    let held: Vec<&str> = rollback
        .removes
        .iter()
        .filter(|loss| !loss.deletes.is_empty())
        .map(|loss| loss.volume.name.as_str())
        .collect();
    let deletes = match held.as_slice() {
        [] => String::new(),
        names => format!(" and deletes the data of {}", names.join(", ")),
    };
    let message = gate.message(&diff, "deploy", || match &undoes {
        Some(undoes) => format!(
            "A human must approve rolling back to #{} first: it undoes {undoes}{deletes}.",
            rollback.number
        ),
        None => "A human must approve this redeploy first.".to_owned(),
    });
    gate.require(bound, message, (&effects, &diff))
        .map_err(|mut refused| {
            if let Some(details) = refused.details.as_object_mut() {
                details.insert(
                    "redeploy".to_owned(),
                    json!({ "deployment": rollback.number, "rolls_back": rollback.rolls_back }),
                );
            }
            refused
        })
}

/// What a Sync or Take moves into an Environment: from which source, and the
/// request's own choices, which its approval covers.
#[derive(Clone, Copy)]
pub(crate) enum Moving<'a> {
    Sync(&'a EnvironmentSummary, &'a serde_json::Value),
    Take(&'a EnvironmentSummary, &'a serde_json::Value),
}

/// A Sync or Take into an Environment that is Protected for the caller, read before
/// it writes: the version Cloud's approval stays fresh against is the one the
/// refusal rolls back to.
pub(crate) struct MoveAsks<'a> {
    gate: Gate<'a>,
    version: String,
}

/// Whether a Sync or Take into `into` asks, read before it writes anything.
///
/// # Errors
/// When the caller's grant can't write `into`.
pub(crate) fn move_asks<'a>(
    tx: &mut dyn Tx,
    into: &Environment,
    trusted: &'a Trusted,
) -> Result<Option<MoveAsks<'a>>, RpcError> {
    let gate = Gate::new(tx, &into.summary.id, trusted)?;
    if !gate.protected {
        return Ok(None);
    }
    trusted.writes(&into.summary.name)?;
    let version = crate::review::review(tx, into)?.view.version;
    Ok(Some(MoveAsks { gate, version }))
}

/// Refuse a Sync or Take into `into`, once written, when [`move_asks`] said it
/// asks, unless Cloud holds a human's approval of exactly this move: its source at
/// its revision, the request's choices, and `into` at the version it held before.
/// The refusal rolls the write back and shows what the move staged.
///
/// # Errors
/// `approval_required`, naming the `version:digest` to approve and `into`'s `diff`.
pub(crate) fn approve_move(
    tx: &mut dyn Tx,
    who: &Actor,
    into: &Environment,
    asks: Option<MoveAsks<'_>>,
    moving: Moving<'_>,
) -> Result<(), RpcError> {
    let Some(MoveAsks { gate, version }) = asks else {
        return Ok(());
    };
    let (what, from, request) = match moving {
        Moving::Sync(from, request) => ("sync", from, request),
        Moving::Take(from, request) => ("take", from, request),
    };
    let bound = json!({
        "organization": who.organization,
        "environment": into.summary.id,
        what: from.id,
        "from_revision": from.revision,
        "request": request,
    });
    let mut diff = crate::review::review(tx, into)?.view;
    diff.version = version;
    let message = gate.message(&diff, what, String::new);
    gate.require(bound, message, (&diff.effects, &diff))
}
