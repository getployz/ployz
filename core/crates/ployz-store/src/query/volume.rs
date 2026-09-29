//! Volumes as `volume ls` and `volume inspect` show them: every Volume in Working
//! State, and every deployed one a staged removal dropped from it, each with its
//! mounts and what the next Deploy does to it. And the Volumes a full Deploy
//! removes, which callers observe the Servers for before admitting it.

use ployz_core::config::{ReviewLifecycleKind, SavedEnvironmentIntent, SavedVolumeIntent};
use ployz_core::{Namespace, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::Actor;
use crate::command::{Mount, VolumeSummary, volume_summary};
use crate::error;
use crate::id::{VolumeId, VolumeName};
use crate::removal::{self, RemovedVolume};
use crate::review;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;

/// Every Volume of an Environment. They are part of one bounded document, so the
/// list comes whole.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumesQuery {
    /// The Environment to list.
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// An Environment's Volumes, by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumesView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// Its Volumes, by name.
    pub volumes: Vec<VolumeListing>,
}

/// One Volume, where it is mounted, and what the next Deploy does to it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeListing {
    /// The Volume.
    #[serde(flatten)]
    pub volume: VolumeSummary,
    /// The Services mounting it in Working State.
    pub mounts: Vec<Mount>,
    /// Whether a Deploy applied it, so the Servers may hold its data.
    pub deployed: bool,
    /// What the next Deploy does to it; none when it is deployed as it is.
    pub change: Option<ReviewLifecycleKind>,
}

/// One Volume by name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumeQuery {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name.
    pub volume: VolumeName,
}

/// One Volume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// The Volume.
    #[serde(flatten)]
    pub volume: VolumeListing,
    /// The lineage its Environment copies share.
    pub lineage: VolumeId,
}

/// The deployed Volumes a full Deploy of an Environment's Working State removes.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemovalsQuery {
    /// The Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// What a full Deploy would remove from the Servers: observe these Docker Volumes
/// and pass what the Servers hold with the admission.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RemovalsView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// The Volumes it removes; empty when a Deploy deletes no data.
    pub volumes: Vec<RemovedVolume>,
}

pub(crate) fn volumes(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &VolumesQuery,
) -> Result<VolumesView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let volumes = listed(tx, &environment)?
        .into_iter()
        .map(|(listing, _)| listing)
        .collect();
    Ok(VolumesView {
        environment: environment.summary,
        volumes,
    })
}

pub(crate) fn volume(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &VolumeQuery,
) -> Result<VolumeView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    // A name removed and added again lists twice; the one in Working State wins.
    let found = listed(tx, &environment)?
        .into_iter()
        .filter(|(listing, _)| listing.volume.name == query.volume)
        .min_by_key(|(listing, _)| listing.change == Some(ReviewLifecycleKind::Delete));
    let Some((listing, node)) = found else {
        return Err(environment
            .volume(&query.volume)
            .err()
            .unwrap_or_else(|| error::corrupt("Volume listing")));
    };
    Ok(VolumeView {
        environment: environment.summary,
        lineage: VolumeId::parse(node.resource_lineage_id.as_str())
            .map_err(|_| error::corrupt("Volume lineage"))?,
        volume: listing,
    })
}

pub(crate) fn removals(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &RemovalsQuery,
) -> Result<RemovalsView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let review = review::review(tx, &environment)?;
    let volumes = if review.head.applied.volumes.is_empty() {
        Vec::new()
    } else {
        // Applied Volumes mean a Deployment was admitted, which fixed the Namespace.
        let namespace: Namespace =
            crate::deployment::namespace(tx, who, &environment.summary, false)?;
        removal::removed(&review.head.applied, &environment.working, &namespace)?
    };
    Ok(RemovalsView {
        environment: environment.summary,
        volumes,
    })
}

/// Working State's Volumes, then deployed ones it dropped, sorted by name.
fn listed(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
) -> Result<Vec<(VolumeListing, SavedVolumeIntent)>, RpcError> {
    let review = review::review(tx, environment)?;
    let working = &environment.working;
    let removed = review
        .head
        .intent
        .volumes
        .iter()
        .filter(|node| {
            working
                .volumes
                .iter()
                .all(|kept| kept.resource_id != node.resource_id)
        })
        .cloned();
    let mut listed = working
        .volumes
        .iter()
        .cloned()
        .chain(removed)
        .map(|node| {
            let listing = VolumeListing {
                volume: volume_summary(&node)?,
                mounts: mounts(working, &node.resource_id)?,
                deployed: review
                    .head
                    .applied
                    .volumes
                    .iter()
                    .any(|applied| applied.resource_id == node.resource_id),
                change: review
                    .view
                    .changes
                    .iter()
                    .find(|change| change.node.id == node.resource_id)
                    .map(|change| change.lifecycle),
            };
            Ok((listing, node))
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    listed.sort_by(|a, b| a.0.volume.name.cmp(&b.0.volume.name));
    Ok(listed)
}

fn mounts(working: &SavedEnvironmentIntent, volume: &str) -> Result<Vec<Mount>, RpcError> {
    let mut mounts = working
        .services
        .iter()
        .flat_map(|service| {
            service
                .volume_attachments
                .iter()
                .filter(|mount| mount.volume_resource_id == volume)
                .map(move |mount| {
                    Ok(Mount {
                        service: ServiceName::parse(service.slug.as_str())
                            .map_err(|_| error::corrupt("Service name"))?,
                        path: mount.mount_path.clone(),
                    })
                })
        })
        .collect::<Result<Vec<_>, RpcError>>()?;
    mounts.sort_by(|a, b| a.service.cmp(&b.service));
    Ok(mounts)
}
