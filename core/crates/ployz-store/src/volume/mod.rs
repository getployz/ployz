//! A Volume's lifecycle in Working State: create it, mount it into Services, detach
//! it and remove it, each staged until a Deploy. Detaching keeps the Volume and its
//! data; only removing a deployed Volume deletes data, and a Deploy of that removal
//! needs its destructive review (see `crate::removal`).

pub(crate) mod query;

use std::collections::BTreeMap;

use ployz_core::config::{SavedEnvironmentIntent, SavedVolumeIntent, VolumeAttachment, VolumeKind};
use ployz_core::{ContainerPath, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, VolumeId, VolumeName};
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
    #[serde(default = "VolumeKind::provisioned_default")]
    pub storage: VolumeKind,
    /// Where Services mount it.
    #[serde(default)]
    pub mounts: Vec<Mount>,
    /// Let more than one container write it; off refuses a second writer.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub shared_writes: bool,
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

/// Rename a Volume: a staged change. Its data and mounts stay; only the name
/// paths use changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RenameVolume {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its current name.
    pub volume: VolumeName,
    /// Its new name, unique among the Environment's Volumes.
    pub name: VolumeName,
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
}

/// Allow or refuse more than one writer of a Volume: replicas of one Service, or
/// several Services. It changes nothing that runs, so it applies at once.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetVolumeSharedWrites {
    /// The Environment containing the Volume.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The Volume, by name.
    pub volume: VolumeName,
    /// On allows several writers; off refuses while it has more than one.
    pub shared_writes: bool,
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
    /// Whether more than one container may write it.
    #[serde(default)]
    pub shared_writes: bool,
}

pub(crate) fn create_volume(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateVolume,
) -> Result<VolumeStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &create.environment)?;
    taken(&environment, &create.name)?;
    let node = SavedVolumeIntent {
        resource_id: create.id.to_string(),
        // A new Volume starts its own lineage; Branch copies keep it.
        resource_lineage_id: create.id.to_string(),
        name: create.name.to_string(),
        storage: create.storage,
        shared_writes: create.shared_writes,
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
        volume: summary(&node)?,
        environment: environment.summary,
        staged,
    })
}

pub(crate) fn set_shared_writes(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetVolumeSharedWrites,
) -> Result<VolumeStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &set.environment)?;
    let before = environment.working.clone();
    environment.volume_mut(&set.volume)?.shared_writes = set.shared_writes;
    if environment.working != before {
        scope::save_working(tx, &mut environment)?;
    }
    // It applies at once: nothing waits for a Deploy.
    staged(environment, &set.volume, false)
}

pub(crate) fn rename_volume(
    tx: &mut dyn Tx,
    who: &Actor,
    rename: &RenameVolume,
) -> Result<VolumeStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &rename.environment)?;
    let changed = rename.name != rename.volume;
    if changed {
        taken(&environment, &rename.name)?;
        environment.volume_mut(&rename.volume)?.name = rename.name.to_string();
        scope::save_working(tx, &mut environment)?;
    }
    staged(environment, &rename.name, changed)
}

pub(crate) fn set_storage(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetVolumeStorage,
) -> Result<VolumeStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &set.environment)?;
    let volume = environment.volume_mut(&set.volume)?;
    let changed = volume.storage != set.storage;
    if changed {
        volume.storage = set.storage;
        check_storage(tx, &environment.summary.id, &environment.working)?;
        scope::save_working(tx, &mut environment)?;
    }
    staged(environment, &set.volume, changed)
}

/// Refuse a second Volume named `name`.
fn taken(environment: &Environment, name: &VolumeName) -> Result<(), RpcError> {
    if environment.volume(name).is_err() {
        return Ok(());
    }
    Err(error::conflict(
        format!(
            "Environment {} already has a Volume named {name}",
            environment.summary.name
        ),
        json!({ "volume": name }),
    ))
}

/// What changing Volume `name` staged: the Volume, when anything changed.
fn staged(
    environment: Environment,
    name: &VolumeName,
    changed: bool,
) -> Result<VolumeStaged, RpcError> {
    Ok(VolumeStaged {
        volume: summary(environment.volume(name)?)?,
        environment: environment.summary,
        staged: changed
            .then(|| SettingPath::volume(name))
            .into_iter()
            .collect(),
    })
}

/// Fix the storage of each Volume of `intent` that `targets` names, the first time
/// a Deployment targets it: an attempt may prepare storage even when it never
/// applies, failed, cancelled and retried ones included. Refuse any other storage.
pub(crate) fn fix_storage(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
    targets: &[crate::deployment::TargetNode],
) -> Result<(), RpcError> {
    for volume in &intent.volumes {
        if !targets.iter().any(|node| node.id() == volume.resource_id) {
            continue;
        }
        let storage = serde_json::to_string(&volume.storage).expect("storage is JSON");
        tx.execute(
            "INSERT INTO config_volume_storage (environment_id, volume_id, storage) \
             VALUES (?1, ?2, ?3) ON CONFLICT (environment_id, volume_id) DO NOTHING",
            &[
                environment.as_str().into(),
                volume.resource_id.as_str().into(),
                storage.as_str().into(),
            ],
        )?;
    }
    check_storage(tx, environment, intent)
}

/// Each Volume's fixed storage, by Volume ID.
pub(crate) fn locked_storage(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeMap<String, VolumeKind>, RpcError> {
    tx.query(
        "SELECT volume_id, storage FROM config_volume_storage WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?
    .iter()
    .map(|row| Ok((row.text(0)?.to_owned(), row.json(1, "Volume storage")?)))
    .collect()
}

/// Refuse `intent` when it gives a Volume storage other than its fixed one.
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
            format!("{service}.mounts.{volume}: expected an absolute path without null characters"),
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
        shared_writes: node.shared_writes,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        Actor, Admit, Change, ConfigStore, CreateProject, CreateService, CreateVolume, Deploy,
        DeploymentId, Edit, EnvironmentId, EnvironmentRef, Mount, OrganizationId, ProjectId,
        ProjectName, ServiceLineageId, SettingPath, Trusted, VolumeId, VolumeName,
    };

    /// Working State saved before shared writes existed can hold two writers of a
    /// Volume without it; only the Store's own storage can write one now.
    #[test]
    fn an_environment_already_sharing_a_volume_keeps_editing_and_deploying() {
        let store =
            ConfigStore::open("sqlite::memory:", crate::SealingKey::new(b"test").unwrap()).unwrap();
        let who = Actor::system(OrganizationId::parse("org").unwrap());
        let uuid = |n: u8| format!("00000000-0000-4000-8000-00000000000{n}");
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse(uuid(1)).unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(3)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse("db").unwrap(),
                    image: Some("postgres:17".into()),
                    template: None,
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateVolume {
                    id: VolumeId::parse(uuid(4)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: VolumeName::parse("data").unwrap(),
                    storage: ployz_core::config::VolumeKind::Docker {},
                    mounts: vec![Mount {
                        service: ployz_core::ServiceName::parse("db").unwrap(),
                        path: "/data".into(),
                    }],
                    shared_writes: false,
                },
            )
            .unwrap();
        crate::rules::tests::seed(&store, &who, |working| {
            for service in &mut working.services {
                service.config.replicas = 2;
            }
        });
        let set = |path: &str, value| {
            store.write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: SettingPath::parse(path).unwrap(),
                        value,
                    }],
                },
            )
        };
        set("db.env.MODE", json!("fast")).unwrap();
        // A third writer is still refused.
        assert_eq!(
            set("db.replicas", json!(3)).unwrap_err().code,
            ployz_core::RpcErrorCode::Conflict
        );
        store
            .write_trusted(
                &who,
                &Admit::Deploy(Deploy {
                    id: DeploymentId::parse(uuid(5)).unwrap(),
                    environment: EnvironmentRef::default(),
                    services: Vec::new(),
                    version: None,
                    upload: None,
                    accept_volume_loss: Vec::new(),
                    message: None,
                }),
                &Trusted::default(),
            )
            .unwrap();
    }
}
