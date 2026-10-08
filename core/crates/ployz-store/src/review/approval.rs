//! Approval: what a review destroys, as Destructive Effects, and the gate that
//! refuses until a human approved exactly that set. Cloud says whether a human must
//! approve; the Store never takes the caller's word for it.

use std::collections::BTreeSet;

use ployz_core::RpcError;
use ployz_core::config::{
    ChangeKind, EnvironmentNodeType, ReviewLifecycleKind, SavedEnvironmentIntent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error;
use crate::id::{ApprovalDigest, EnvironmentId};
use crate::review::{NodeChange, Review};
use crate::{Actor, Approval};

/// One thing a publication destroys, by the node it changes and the row.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
pub struct DestructiveEffect {
    /// What it destroys.
    pub kind: DestructiveKind,
    /// What a human calls the thing destroyed: the Service, the Volume, or the
    /// domain's hostname.
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

pub(crate) fn destructive_effects(
    changes: &[NodeChange],
    applied: &SavedEnvironmentIntent,
) -> BTreeSet<DestructiveEffect> {
    let mut effects = BTreeSet::new();
    for change in changes {
        let node = &change.node.id;
        let effect = |kind, name: &str, path: &str| DestructiveEffect {
            kind,
            name: name.to_owned(),
            node: node.clone(),
            path: path.to_owned(),
        };
        let removed = change.lifecycle == ReviewLifecycleKind::Delete;
        match change.node.node_type {
            EnvironmentNodeType::Volume => {
                let deployed = applied
                    .volumes
                    .iter()
                    .any(|volume| volume.resource_id == *node);
                if deployed && removed {
                    let path = format!("volumes.{}", change.name);
                    effects.insert(effect(DestructiveKind::DeletesVolume, &change.name, &path));
                }
            }
            EnvironmentNodeType::Service => {
                if !applied.services.iter().any(|service| service.id == *node) {
                    continue;
                }
                if removed {
                    effects.insert(effect(
                        DestructiveKind::RemovesService,
                        &change.name,
                        &change.name,
                    ));
                    continue;
                }
                let routes = format!("{}.routes.", change.name);
                for row in &change.settings {
                    if let Some((_, volume)) = row.path.split_once(".mounts.")
                        && row.after.is_null()
                    {
                        effects.insert(effect(DestructiveKind::DetachesVolume, volume, &row.path));
                    } else if row.path.starts_with(&routes) && row.kind == ChangeKind::Remove {
                        let hostname = row
                            .before
                            .get("hostname")
                            .and_then(Value::as_str)
                            .unwrap_or(&row.path);
                        effects.insert(effect(DestructiveKind::RemovesDomain, hostname, &row.path));
                    }
                }
            }
        }
    }
    drop_detaches_of_deleted_volumes(&mut effects);
    effects
}

fn drop_detaches_of_deleted_volumes(effects: &mut BTreeSet<DestructiveEffect>) {
    let deleted: BTreeSet<String> = effects
        .iter()
        .filter(|effect| effect.kind == DestructiveKind::DeletesVolume)
        .map(|effect| effect.name.clone())
        .collect();
    effects.retain(|effect| {
        effect.kind != DestructiveKind::DetachesVolume || !deleted.contains(&effect.name)
    });
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
