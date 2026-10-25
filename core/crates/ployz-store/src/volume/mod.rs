//! A Volume's lifecycle in Working State: create it, mount it into Services, detach
//! it and remove it, each staged until a Deploy. Detaching keeps the Volume and its
//! data; only removing a deployed Volume deletes data, and a Deploy of that removal
//! needs its destructive review (see `crate::removal`).

pub(crate) mod query;
pub(crate) mod size;

use std::collections::BTreeMap;

use ployz_core::config::{
    RestoredFrom, SavedEnvironmentIntent, SavedServiceIntent, SavedVolumeIntent, VolumeAttachment,
    VolumeKind,
};
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
pub(crate) use size::{StorageRule, check_storage};

/// Create a Volume, optionally mounted into Services.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateVolume {
    /// The new Volume's ID, also its lineage.
    pub id: VolumeId,
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
    /// Fix its storage now, as a Deploy would. A restored Volume keeps the storage of the copy it lands on.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub storage_locked: bool,
    /// The restore point of another Volume in the Environment that its data comes
    /// from. A review then says which of the source's changes it loses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub restored_from: Option<RestoredFrom>,
}

/// One Service mounting a Volume at a path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub service: ServiceName,
    /// The absolute path in its containers.
    pub path: String,
}

/// Remove a Volume from Working State, detaching it from every Service. Its data
/// stays on the Servers until a Deploy of the removal, which asks to accept the loss.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveVolume {
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub volume: VolumeName,
}

/// Rename a Volume: a staged change. Its data and mounts stay; only the name
/// paths use changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RenameVolume {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its current name.
    pub volume: VolumeName,
    /// Its new name, unique among the Environment's Volumes.
    pub name: VolumeName,
}

/// Change a draft Volume's storage. A Deploy fixes its storage choice and limit; a
/// deployed Managed Volume then resizes only through `ployz volume set NAME --size N`,
/// which resizes every copy and records the size.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetVolumeStorage {
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
    #[serde(default)]
    pub environment: EnvironmentRef,
    pub volume: VolumeName,
    /// On allows several writers; off refuses while it has more than one.
    pub shared_writes: bool,
}

/// A Volume created or removed in Working State, staged until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeStaged {
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
    if let Some(from) = &create.restored_from
        && environment
            .working
            .volumes
            .iter()
            .all(|volume| volume.resource_id != from.volume)
    {
        return Err(error::not_found(
            "The Volume to restore from is not in this Environment.",
            json!({"restored_from": from.volume}),
        ));
    }
    crate::names::refuse_taken(
        tx,
        &environment,
        crate::names::Name::Volume(create.name.as_str()),
        None,
    )?;
    let node = SavedVolumeIntent {
        resource_id: create.id.to_string(),
        // A new Volume starts its own lineage; Branch copies keep it.
        resource_lineage_id: create.id.to_string(),
        name: create.name.to_string(),
        storage: create.storage,
        shared_writes: create.shared_writes,
        restored_from: create.restored_from.clone(),
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
    for mount in &create.mounts {
        refuse_shared_path(&environment, &mount.service, &create.name)?;
    }
    scope::save_working(tx, &mut environment)?;
    scope::introduce(tx, who, &environment.summary.id, scope::Node::Volume(&node))?;
    if create.storage_locked {
        lock_storage(
            tx,
            &environment.summary.id,
            &node.resource_id,
            &node.storage,
        )?;
    }
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
        crate::names::refuse_taken(
            tx,
            &environment,
            crate::names::Name::Volume(rename.name.as_str()),
            None,
        )?;
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
        let edited = volume.resource_id.clone();
        check_storage(
            tx,
            &environment.summary.id,
            &environment.working.volumes,
            StorageRule::Edit { volume: &edited },
        )?;
        scope::save_working(tx, &mut environment)?;
    }
    staged(environment, &set.volume, changed)
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

/// Refuse storage of `intent` that a Volume's fixed storage doesn't fit, under
/// [`StorageRule::Replay`], before a Deployment of it is admitted.
pub(crate) fn check_admitted(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    check_storage(tx, environment, &intent.volumes, StorageRule::Replay)
}

/// Fix the storage of each Volume of `intent` that the Deploy Preview a run is about to
/// execute may prepare, failed, cancelled and retried runs included. A Volume no run
/// prepared holds no data on any Server, so its storage stays editable.
pub(crate) fn fix_prepared(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
    prepared: impl Fn(&str) -> bool,
) -> Result<(), RpcError> {
    for volume in &intent.volumes {
        if prepared(&volume.resource_id) {
            lock_storage(tx, environment, &volume.resource_id, &volume.storage)?;
        }
    }
    Ok(())
}

/// Fix `volume`'s storage at `storage`, unless it is already fixed.
fn lock_storage(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    volume: &str,
    storage: &VolumeKind,
) -> Result<(), RpcError> {
    let storage = serde_json::to_string(storage).expect("storage is JSON");
    tx.execute(
        "INSERT INTO config_volume_storage (environment_id, volume_id, storage) \
         VALUES (?1, ?2, ?3) ON CONFLICT (environment_id, volume_id) DO NOTHING",
        &[
            environment.as_str().into(),
            volume.into(),
            storage.as_str().into(),
        ],
    )?;
    Ok(())
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
    if let Some(refused) = kernel_refusal(service.as_str(), volume.as_str(), &mount) {
        return Err(refused);
    }
    node.volume_attachments
        .retain(|mount| mount.volume_resource_id != id);
    node.volume_attachments.push(mount);
    Ok(true)
}

/// Refuse `service` mounting Volume `volume` where it already mounts another Volume
/// or a Config. Judged once a whole write is applied, so mounts that trade places
/// in one edit pass.
pub(crate) fn refuse_shared_path(
    environment: &Environment,
    service: &ServiceName,
    volume: &VolumeName,
) -> Result<(), RpcError> {
    let id = &environment.volume(volume)?.resource_id;
    let node = environment.service(service)?;
    match node
        .volume_attachments
        .iter()
        .find(|mount| &mount.volume_resource_id == id)
    {
        Some(mount) => shared_path(&environment.working, node, mount).map_or(Ok(()), Err),
        None => Ok(()),
    }
}

/// Refuse a Volume mount `after` breaks that neither `before` nor `from` already
/// broke: one under a kernel filesystem, or one where its Service mounts something
/// else. Whatever write made it, a Discard or a Sync among them. The refusal names
/// the clash from the mount `before` lacks.
pub(crate) fn refuse_new_mounts(
    before: &SavedEnvironmentIntent,
    from: Option<&SavedEnvironmentIntent>,
    after: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let held: Vec<Found> = std::iter::once(before)
        .chain(from)
        .flat_map(broken)
        .collect();
    let mounted: Vec<MountKey> = mounts(before).map(|(key, _)| key).collect();
    let new: Vec<Found> = broken(after)
        .into_iter()
        .filter(|found| held.iter().all(|held| held.rule != found.rule))
        .collect();
    let mounted_before = |found: &Found| mounted.contains(&found.mount);
    match new
        .iter()
        .find(|found| !mounted_before(found))
        .or(new.first())
    {
        Some(found) => Err(found.refused.clone()),
        None => Ok(()),
    }
}

/// A mount rule an intent breaks, by lineage, so an Environment and its Branch agree.
#[derive(Debug, PartialEq, Eq)]
enum Broken {
    Kernel { service: String, at: String },
    Shared { service: String, at: String },
}

/// A mount's Service and Volume, by lineage, and its container path.
type MountKey = (String, String, String);

struct Found {
    rule: Broken,
    mount: MountKey,
    refused: RpcError,
}

/// Each Volume mount of `intent` with its key and Service.
fn mounts(
    intent: &SavedEnvironmentIntent,
) -> impl Iterator<
    Item = (
        MountKey,
        (&SavedServiceIntent, &SavedVolumeIntent, &VolumeAttachment),
    ),
> {
    intent.services.iter().flat_map(move |node| {
        node.volume_attachments.iter().filter_map(move |mount| {
            let volume = intent
                .volumes
                .iter()
                .find(|volume| volume.resource_id == mount.volume_resource_id)?;
            let key = (
                node.lineage_id.clone(),
                volume.resource_lineage_id.clone(),
                mount.container_path(),
            );
            Some((key, (node, volume, mount)))
        })
    })
}

fn broken(intent: &SavedEnvironmentIntent) -> Vec<Found> {
    let mut found = Vec::new();
    for (key, (node, volume, mount)) in mounts(intent) {
        let service = node.lineage_id.clone();
        let at = key.2.clone();
        if let Some(refused) = kernel_refusal(&node.slug, &volume.name, mount) {
            let rule = Broken::Kernel {
                service: service.clone(),
                at: at.clone(),
            };
            found.push(Found {
                rule,
                mount: key.clone(),
                refused,
            });
        }
        if let Some(refused) = shared_path(intent, node, mount) {
            let rule = Broken::Shared { service, at };
            found.push(Found {
                rule,
                mount: key,
                refused,
            });
        }
    }
    found
}

/// The refusal of `mount` on `service` when it lands under a kernel filesystem.
fn kernel_refusal(service: &str, volume: &str, mount: &VolumeAttachment) -> Option<RpcError> {
    let at = mount.container_path();
    let kernel = crate::config_item::kernel_filled(&at)?;
    Some(error::invalid(
        format!(
            "{service}.mounts.{volume}: {at} is under {kernel}, which the kernel fills, so it can't hold a Volume"
        ),
        json!({ "example": "/var/lib/data" }),
    ))
}

/// The refusal of `mount` on `node` when `node` mounts another Volume or a Config
/// where it lands in `working`.
fn shared_path(
    working: &SavedEnvironmentIntent,
    node: &SavedServiceIntent,
    mount: &VolumeAttachment,
) -> Option<RpcError> {
    let at = mount.container_path();
    let same_volume = node
        .volume_attachments
        .iter()
        .filter(|held| {
            held.volume_resource_id != mount.volume_resource_id && held.container_path() == at
        })
        .find_map(|held| {
            working
                .volumes
                .iter()
                .find(|other| other.resource_id == held.volume_resource_id)
                .map(|other| format!("Volume {}", other.name))
        });
    let same_config = node
        .config_attachments
        .iter()
        .filter(|held| held.mount_dir.as_str() == at)
        .find_map(|held| {
            working
                .configs
                .iter()
                .find(|config| config.resource_id == held.config_resource_id)
                .map(|config| format!("Config {}", config.name))
        });
    let what = same_volume.or(same_config)?;
    let service = &node.slug;
    Some(error::conflict(
        format!("{service} already mounts {what} at {at}; pick another directory"),
        json!({ "service": service, "dir": at }),
    ))
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
                    storage_locked: false,
                    restored_from: None,
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
                    prune: false,
                    no_prune: false,
                }),
                &Trusted::default(),
            )
            .unwrap();
    }
}
