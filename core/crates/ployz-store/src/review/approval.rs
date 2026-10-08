//! Approval: what a review destroys, as Destructive Effects, and the gate that
//! refuses until a human approved exactly that set. Cloud says whether a human must
//! approve; the Store never takes the caller's word for it.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::SavedEnvironmentIntent;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::id::{ApprovalDigest, EnvironmentId, Hostname};
use crate::review::{Head, Review};
use crate::{Actor, Approval};

/// One thing a publication destroys, by the node it changes and the row.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
pub struct DestructiveEffect {
    /// What it destroys.
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
}

impl DestructiveEffect {
    fn describe(&self) -> String {
        let name = &self.name;
        match self.kind {
            DestructiveKind::RemovesService => format!("remove Service {name}"),
            DestructiveKind::DeletesVolume => format!("delete Volume {name}"),
            DestructiveKind::DetachesVolume => match self.path.split_once(".mounts.") {
                Some((service, _)) => format!("detach Volume {name} from {service}"),
                None => format!("detach Volume {name}"),
            },
            DestructiveKind::RemovesDomain => format!("remove domain {name}"),
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
            });
        }
    }
    for deployed in &running.services {
        let effect = |kind, name: &str, path: String| DestructiveEffect {
            kind,
            name: name.to_owned(),
            node: deployed.id.clone(),
            path,
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
        for attachment in &deployed.volume_attachments {
            let id = &attachment.volume_resource_id;
            let mounted = service
                .volume_attachments
                .iter()
                .any(|kept| kept.volume_resource_id == *id);
            let kept = target
                .volumes
                .iter()
                .find(|volume| volume.resource_id == *id);
            if let Some(volume) = kept
                && !mounted
            {
                let path = format!("{slug}.mounts.{}", volume.name);
                effects.insert(effect(DestructiveKind::DetachesVolume, &volume.name, path));
            }
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

pub(crate) fn approval_digest(
    who: &Actor,
    environment: &EnvironmentId,
    version: &str,
    effects: &BTreeSet<DestructiveEffect>,
) -> ApprovalDigest {
    let bound = json!({
        "organization": who.organization,
        "environment": environment,
        "effects": effects,
    });
    let digest = crate::removal::short_digest(&bound.to_string());
    ApprovalDigest::parse(format!("{version}:{digest}"))
        .expect("a version and a 16-digit hex digest form an approval")
}

/// Refuse publishing `target` while it destroys something, unless no approval is
/// required or Cloud holds a human's approval of exactly this version and set.
/// Publishing already-Saved State asks nothing: the Publish that saved it did.
///
/// # Errors
/// `approval_required`, naming the effects, the `version:digest` to approve and the
/// fresh `diff`.
pub(crate) fn approve(
    who: &Actor,
    environment: &EnvironmentId,
    review: &Review,
    target: &SavedEnvironmentIntent,
    trusted: &Approval,
) -> Result<(), RpcError> {
    let diff = &review.view;
    let effects = &diff.effects;
    let resaves = review
        .saved
        .as_ref()
        .is_some_and(|saved| saved.intent == *target);
    match trusted {
        Approval::NotRequired => return Ok(()),
        Approval::Required | Approval::Approved(_) => {}
    }
    if effects.is_empty() || resaves {
        return Ok(());
    }
    let digest = approval_digest(who, environment, &diff.version, effects);
    if matches!(trusted, Approval::Approved(approved) if *approved == digest) {
        return Ok(());
    }
    let asked = effects
        .iter()
        .map(DestructiveEffect::describe)
        .collect::<Vec<_>>()
        .join(", ");
    Err(error::approval_required(
        format!("A human must approve this first: {asked}"),
        json!({"effects": effects, "approval": digest, "diff": diff}),
    ))
}
