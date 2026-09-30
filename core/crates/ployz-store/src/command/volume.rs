//! A Volume's lifecycle in Working State: create it, mount it into Services, detach
//! it and remove it, each staged until a Deploy. Detaching keeps the Volume and its
//! data; only removing a deployed Volume deletes data, and a Deploy of that removal
//! needs its destructive review (see `crate::removal`).

use std::collections::BTreeMap;

use ployz_core::config::{SavedEnvironmentIntent, SavedVolumeIntent, VolumeAttachment, VolumeKind};
use ployz_core::{ContainerPath, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{Command, replayable};
use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, Revision, VolumeId, VolumeName};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
use crate::settings::{SettingPath, Target};
use crate::storage::Tx;

/// Create a Volume, optionally mounted into Services.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateVolume {
    /// The new Volume's ID, also its lineage.
    pub id: VolumeId,
    /// The Environment to create it in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name, unique among the Environment's Volumes.
    pub name: VolumeName,
    /// Managed storage by default; Docker storage is an explicit opt-out.
    #[serde(default = "VolumeKind::managed_default")]
    pub storage: VolumeKind,
    /// Where Services mount it.
    #[serde(default)]
    pub mounts: Vec<Mount>,
}

/// One Service mounting a Volume at a path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// The Service, by name.
    pub service: ServiceName,
    /// The absolute path in its containers.
    pub path: String,
}

/// Remove a Volume from Working State, detaching it from every Service. Its data
/// stays on the Servers until a Deploy of the removal, which asks to accept the loss.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveVolume {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name.
    pub volume: VolumeName,
}

/// Change a draft Volume's storage. Deployment fixes its storage choice and limit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetVolumeStorage {
    /// The Environment containing the Volume.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The draft Volume to edit, by name.
    pub volume: VolumeName,
    /// Its explicit storage choice and bound.
    pub storage: VolumeKind,
    #[serde(default)]
    /// Refuse edits against a different Working revision.
    pub expect: Option<Revision>,
}

/// A Volume created or removed in Working State, staged until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeStaged {
    /// The Volume.
    pub volume: VolumeSummary,
    /// The Environment, at its revision after the change.
    pub environment: EnvironmentSummary,
    /// What waits for a Deploy: the Volume as `volumes.NAME`, and each mount it
    /// gained or lost as `SERVICE.mounts.NAME`.
    pub staged: Vec<SettingPath>,
    /// What took effect at once: never anything here.
    pub immediate: Vec<SettingPath>,
}

/// A Volume as results name it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeSummary {
    /// Its durable identity.
    pub id: VolumeId,
    /// Its name, which mount paths address it by.
    pub name: VolumeName,
    /// Its chosen storage, including the maximum for a Provisioned Volume.
    pub storage: VolumeKind,
}

pub(crate) fn create_volume(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateVolume,
) -> Result<VolumeStaged, RpcError> {
    let command = Command::CreateVolume(create.clone());
    replayable(tx, who, &command, |tx| {
        let mut environment = scope::lock(tx, who, &create.environment)?;
        if environment.volume(&create.name).is_ok() {
            return Err(error::conflict(
                format!(
                    "Environment {} already has a Volume named {}",
                    environment.summary.name, create.name
                ),
                json!({ "volume": create.name }),
            ));
        }
        let node = SavedVolumeIntent {
            resource_id: create.id.to_string(),
            // A new Volume starts its own lineage; Branch copies keep it.
            resource_lineage_id: create.id.to_string(),
            name: create.name.to_string(),
            storage: create.storage,
        };
        environment.working.volumes.push(node.clone());
        let mut staged = vec![SettingPath::volume(&create.name)];
        for mount in &create.mounts {
            if attach(&mut environment, &mount.service, &create.name, &mount.path)? {
                staged.push(SettingPath::at(
                    &mount.service,
                    Target::Mount(create.name.clone()),
                ));
            }
        }
        scope::save_working(tx, &mut environment)?;
        scope::introduce(tx, who, &environment.summary.id, scope::Node::Volume(&node))?;
        Ok(VolumeStaged {
            volume: VolumeSummary {
                id: create.id.clone(),
                name: create.name.clone(),
                storage: create.storage,
            },
            environment: environment.summary,
            staged,
            immediate: Vec::new(),
        })
    })
}

pub(crate) fn set_storage(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetVolumeStorage,
) -> Result<VolumeStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &set.environment)?;
    environment.expect(set.expect)?;
    let node = environment.volume(&set.volume)?;
    let changed = node.storage != set.storage;
    let id = node.resource_id.clone();
    if changed {
        environment
            .working
            .volumes
            .iter_mut()
            .find(|node| node.resource_id == id)
            .expect("Volume was found")
            .storage = set.storage;
        scope::save_working(tx, &mut environment)?;
    }
    Ok(VolumeStaged {
        volume: summary(environment.volume(&set.volume)?)?,
        environment: environment.summary,
        staged: if changed {
            vec![SettingPath::volume(&set.volume)]
        } else {
            Vec::new()
        },
        immediate: Vec::new(),
    })
}

/// An admitted Deployment can prepare storage even when it never applies a node.
/// Keep its storage choice fixed, including failed and cancelled attempts and retries.
pub(crate) fn locked_storage(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeMap<String, VolumeKind>, RpcError> {
    // ponytail: scan this Environment's attempt history; materialize first storage choices if history makes this costly.
    let rows = tx.query(
        "SELECT s.intent, d.nodes FROM config_deployment d JOIN config_saved s \
         ON s.environment_id = d.environment_id AND s.revision = d.saved_revision \
         WHERE d.environment_id = ?1 ORDER BY d.number",
        &[environment.as_str().into()],
    )?;
    let mut locked = BTreeMap::new();
    for row in rows {
        let saved = row.intent(0, "Saved Volume storage")?;
        let nodes: Vec<crate::deployment::TargetNode> = row.json(1, "Deployment nodes")?;
        for volume in saved.volumes {
            if nodes.iter().any(|node| node.id() == volume.resource_id) {
                locked.entry(volume.resource_id).or_insert(volume.storage);
            }
        }
    }
    Ok(locked)
}

pub(crate) fn check_storage(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    if intent.volumes.is_empty() {
        return Ok(());
    }
    let locked = locked_storage(tx, environment)?;
    for volume in &intent.volumes {
        if locked
            .get(&volume.resource_id)
            .is_some_and(|storage| *storage != volume.storage)
        {
            return Err(error::conflict(
                format!(
                    "Volume {} storage cannot change after deployment has been requested",
                    volume.name
                ),
                json!({ "volume": volume.name, "storage_locked": true }),
            ));
        }
    }
    Ok(())
}

pub(crate) fn remove_volume(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &RemoveVolume,
) -> Result<VolumeStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &remove.environment)?;
    let volume = summary(environment.volume(&remove.volume)?)?;
    let mut staged = vec![SettingPath::volume(&volume.name)];
    for service in &mut environment.working.services {
        let before = service.volume_attachments.len();
        service
            .volume_attachments
            .retain(|mount| mount.volume_resource_id != volume.id.as_str());
        if service.volume_attachments.len() != before {
            let name =
                ServiceName::parse(service.slug.as_str()).map_err(|_| error::corrupt("Service"))?;
            staged.push(SettingPath::at(&name, Target::Mount(volume.name.clone())));
        }
    }
    environment
        .working
        .volumes
        .retain(|node| node.resource_id != volume.id.as_str());
    scope::save_working(tx, &mut environment)?;
    Ok(VolumeStaged {
        volume,
        environment: environment.summary,
        staged,
        immediate: Vec::new(),
    })
}

/// Mount Volume `volume` into `service` at `path`, replacing where it was mounted.
/// Returns whether Working State changed.
pub(crate) fn attach(
    environment: &mut Environment,
    service: &ServiceName,
    volume: &VolumeName,
    path: &str,
) -> Result<bool, RpcError> {
    let path = ContainerPath::parse(path).map_err(|_| {
        error::invalid(
            format!("{service}.mounts.{volume}: expected an absolute path"),
            json!({ "example": "/var/lib/data" }),
        )
    })?;
    let id = environment.volume(volume)?.resource_id.clone();
    let node = environment.service_mut(service)?;
    let mount = VolumeAttachment {
        volume_resource_id: id.clone(),
        mount_path: path.as_str().to_owned(),
    };
    if node.volume_attachments.contains(&mount) {
        return Ok(false);
    }
    node.volume_attachments
        .retain(|mount| mount.volume_resource_id != id);
    node.volume_attachments.push(mount);
    Ok(true)
}

/// Detach Volume `volume` from `service`; the Volume and its data stay. Returns
/// whether Working State changed.
pub(crate) fn detach(
    environment: &mut Environment,
    service: &ServiceName,
    volume: &VolumeName,
) -> Result<bool, RpcError> {
    let id = environment.volume(volume)?.resource_id.clone();
    let node = environment.service_mut(service)?;
    let before = node.volume_attachments.len();
    node.volume_attachments
        .retain(|mount| mount.volume_resource_id != id);
    Ok(node.volume_attachments.len() != before)
}

pub(crate) fn summary(node: &SavedVolumeIntent) -> Result<VolumeSummary, RpcError> {
    Ok(VolumeSummary {
        id: VolumeId::parse(node.resource_id.as_str()).map_err(|_| error::corrupt("Volume ID"))?,
        name: VolumeName::parse(node.name.as_str()).map_err(|_| error::corrupt("Volume name"))?,
        storage: node.storage,
    })
}
