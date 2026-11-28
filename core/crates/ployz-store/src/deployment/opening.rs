//! A preview's Deployment as its preview rule approves it: the Parent's latest Saved
//! revision run in the preview's own Namespace. The PR Environment's Own Copies have IDs of
//! their own and no Sealed values, but its specs name what the Parent's do, so one signature
//! covers every preview of the Parent: each node lowers under the Parent's ID with the
//! Parent's settings and Sealed values, sealed for the Parent; only its source is the
//! preview's. Each Volume is the preview's own, born empty, each generated domain names the
//! preview, and custom domains, extra replicas and Setup Commands stay behind. Every Server
//! checks all of it against the rule; nothing here admits a Container.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::{DockerVolumeName, MachineId, Namespace, PreviewNamespace, RpcError, VolumeBirth};
use serde_json::json;

use crate::error;
use crate::id::EnvironmentId;
use crate::storage::Tx;

/// Where a preview's Deployment opens from: its Parent and what the Parent saved last.
pub(crate) struct Opening {
    /// The Environment whose Sealed values the preview opens, and whose IDs its specs carry.
    pub(crate) parent: EnvironmentId,
    parent_saved: SavedEnvironmentIntent,
    preview: PreviewNamespace,
}

/// A preview's Saved revision as its Parent's copy, and what [`super::lower`] reads beside
/// it, keyed by the Parent's IDs.
pub(super) struct Opened {
    pub(super) intent: SavedEnvironmentIntent,
    pub(super) deployed: BTreeSet<String>,
    pub(super) born: BTreeMap<String, VolumeBirth>,
    pub(super) first_copies: BTreeMap<String, MachineId>,
}

impl Opening {
    /// How `environment`'s Deployments into `namespace` open, when `namespace` is a preview's:
    /// from the Branch's Parent. None for any other Deployment.
    ///
    /// # Errors
    /// Returns `invalid_argument` when the Parent has saved nothing, so a claim fails rather
    /// than queue, or a storage error.
    pub(crate) fn of(
        tx: &mut dyn Tx,
        environment: &EnvironmentId,
        namespace: &Namespace,
    ) -> Result<Option<Self>, RpcError> {
        let Some(preview) = PreviewNamespace::of(namespace) else {
            return Ok(None);
        };
        let Some(parent) = crate::branch::rows::parent_of(tx, environment.as_str())? else {
            return Ok(None);
        };
        let parent = EnvironmentId::parse(parent)?;
        let parent_saved = crate::review::latest_saved(tx, &parent)?
            .ok_or_else(|| {
                error::invalid(
                    "The environment previews start from has nothing saved, so a preview has nothing to copy",
                    json!({}),
                )
            })?
            .intent;
        Ok(Some(Self {
            parent,
            parent_saved,
            preview,
        }))
    }

    /// The opening of the Parent's own latest Saved revision as a preview in `preview`: what
    /// a preview rule is drafted from.
    pub(crate) fn sample(
        parent: EnvironmentId,
        parent_saved: SavedEnvironmentIntent,
        preview: PreviewNamespace,
    ) -> Self {
        Self {
            parent,
            parent_saved,
            preview,
        }
    }

    /// `saved`, a revision of the preview, as its Parent's copy: each node of the Parent's
    /// latest Saved revision the preview has, with the source the preview's copy tracks,
    /// and `deployed` and `first_copies`, keyed by the preview's IDs, rekeyed by the
    /// Parent's.
    pub(super) fn open(
        &self,
        saved: &SavedEnvironmentIntent,
        (deployed, first_copies): (&BTreeSet<String>, &BTreeMap<String, MachineId>),
    ) -> Opened {
        let mut ids: BTreeMap<String, String> = BTreeMap::new();
        let mut intent = self.parent_saved.clone();
        intent.services.retain_mut(|service| {
            let Some(ours) =
                (saved.services.iter()).find(|ours| ours.lineage_id == service.lineage_id)
            else {
                return false;
            };
            ids.insert(ours.id.clone(), service.id.clone());
            service.config.source = ours.config.source.clone();
            true
        });
        intent.volumes.retain(|volume| {
            let ours = (saved.volumes.iter())
                .find(|ours| ours.resource_lineage_id == volume.resource_lineage_id);
            if let Some(ours) = ours {
                ids.insert(ours.resource_id.clone(), volume.resource_id.clone());
            }
            ours.is_some()
        });
        intent.configs.retain(|config| {
            (saved.configs.iter())
                .any(|ours| ours.resource_lineage_id == config.resource_lineage_id)
        });
        let namespace = self.preview.namespace();
        for service in &mut intent.services {
            service.config.replicas = service.config.replicas.only(std::num::NonZeroU8::MIN);
            service.config.routes.clear();
            for managed in &mut service.config.managed_hostnames {
                managed.prefix = preview_prefix(&managed.prefix, namespace);
            }
        }
        let born = intent
            .volumes
            .iter()
            .filter_map(|volume| {
                let declared = DockerVolumeName::for_resource(&volume.resource_id).ok()?;
                Some((
                    volume.resource_id.clone(),
                    self.preview.birth(declared.as_str()),
                ))
            })
            .collect();
        let theirs = |id: &String| ids.get(id).cloned().unwrap_or_else(|| id.clone());
        Opened {
            intent,
            deployed: deployed.iter().map(theirs).collect(),
            born,
            first_copies: first_copies
                .iter()
                .map(|(id, machine)| (theirs(id), *machine))
                .collect(),
        }
    }
}

/// The label a generated domain `prefix` takes in preview `namespace`: the prefix, cut to
/// fit one DNS label, then the Namespace, which each Server checks the hostname names.
fn preview_prefix(prefix: &str, namespace: &Namespace) -> String {
    let room = 63usize.saturating_sub(namespace.as_str().len() + 1);
    let cut = prefix.get(..room).unwrap_or(prefix).trim_end_matches('-');
    if cut.is_empty() {
        return namespace.as_str().to_owned();
    }
    format!("{cut}-{namespace}")
}
