//! A Volume's lifecycle in Working State: create it, mount it into Services, detach
//! it and remove it, each staged until a Deploy. Detaching keeps the Volume and its
//! data; only removing a deployed Volume deletes data, and a Deploy of that removal
//! needs its destructive review (see `crate::removal`).

use ployz_core::config::{SavedVolumeIntent, VolumeAttachment};
use ployz_core::{ContainerPath, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{Command, replayable};
use crate::Actor;
use crate::error;
use crate::id::{VolumeId, VolumeName};
use crate::scope::{self, Environment, EnvironmentRef, EnvironmentSummary};
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

/// A Volume created or removed in Working State, staged until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeStaged {
    /// The Volume.
    pub volume: VolumeSummary,
    /// The Environment, at its revision after the change.
    pub environment: EnvironmentSummary,
    /// What waits for a Deploy: the Volume as `volumes.NAME`, and each mount it
    /// gained or lost as `SERVICE.mounts.NAME`.
    pub staged: Vec<String>,
    /// What took effect at once: never anything here.
    pub immediate: Vec<String>,
}

/// A Volume as results name it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeSummary {
    /// Its durable identity.
    pub id: VolumeId,
    /// Its name, which mount paths address it by.
    pub name: VolumeName,
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
        };
        environment.working.volumes.push(node.clone());
        let mut staged = vec![whole(&create.name)];
        for mount in &create.mounts {
            if attach(&mut environment, &mount.service, &create.name, &mount.path)? {
                staged.push(path(&mount.service, &create.name));
            }
        }
        scope::save_working(tx, &mut environment)?;
        scope::introduce(tx, who, &environment.summary.id, scope::Node::Volume(&node))?;
        Ok(VolumeStaged {
            volume: VolumeSummary {
                id: create.id.clone(),
                name: create.name.clone(),
            },
            environment: environment.summary,
            staged,
            immediate: Vec::new(),
        })
    })
}

pub(crate) fn remove_volume(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &RemoveVolume,
) -> Result<VolumeStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &remove.environment)?;
    let volume = summary(environment.volume(&remove.volume)?)?;
    let mut staged = vec![whole(&volume.name)];
    for service in &mut environment.working.services {
        let before = service.volume_attachments.len();
        service
            .volume_attachments
            .retain(|mount| mount.volume_resource_id != volume.id.as_str());
        if service.volume_attachments.len() != before {
            staged.push(format!("{}.mounts.{}", service.slug, volume.name));
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
    })
}

fn whole(volume: &VolumeName) -> String {
    format!("volumes.{volume}")
}

fn path(service: &ServiceName, volume: &VolumeName) -> String {
    format!("{service}.mounts.{volume}")
}
