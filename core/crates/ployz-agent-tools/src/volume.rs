//! Tools on Volumes: create one, change one, remove one.

use ployz_core::config::VolumeKind;
use ployz_core::{ProvisionedVolumeMaximumBytes, ServiceName};
use serde_json::{Value, json};

use super::ToolId;
use super::input::tool_input;
use super::machine::{Cx, Effect, Stop, Tool, effects_ask, written};
use super::refusal::Refusal;
use ployz_store::catalog::NODE_NAME;
use ployz_store::{
    BatchCommand, CreateVolume, DestructiveKind, Mount, RemoveVolume, RenameVolume,
    SetVolumeSharedWrites, SetVolumeStorage, VolumeName, VolumeStaged, VolumeSummary, VolumesQuery,
    Written,
};

const SIZE: &str = "^[0-9]+(GB|GiB|TB|TiB)$";

/// `size` in bytes: `10GiB`, `1TB`.
fn bytes(size: &str, field: &str) -> Result<VolumeKind, Stop> {
    let invalid = || {
        Stop::from(Refusal::invalid(
            "A size is a whole number of GB, GiB, TB or TiB",
            field,
        ))
    };
    let split = size
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(invalid)?;
    let (number, unit) = size.split_at(split);
    let unit = match unit {
        "GB" => 1_000_000_000,
        "GiB" => 1 << 30,
        "TB" => 1_000_000_000_000,
        "TiB" => 1 << 40,
        _ => return Err(invalid()),
    };
    let maximum_bytes = number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(unit))
        .and_then(|bytes| ProvisionedVolumeMaximumBytes::try_from(bytes).ok())
        .ok_or_else(invalid)?;
    Ok(VolumeKind::Provisioned { maximum_bytes })
}

/// `maximum_bytes` in the unit it was most likely given in: `1 GiB`, `2 TiB`, `1.5 GB`.
fn size(maximum_bytes: ProvisionedVolumeMaximumBytes) -> String {
    let bytes = maximum_bytes.get();
    match [(1_u64 << 40, "TiB"), (1 << 30, "GiB")]
        .into_iter()
        .find(|(unit, _)| bytes.is_multiple_of(*unit))
    {
        Some((unit, name)) => format!("{} {name}", bytes / unit),
        None => maximum_bytes.to_string(),
    }
}

/// A Volume as a result shows it.
fn volume(volume: &VolumeSummary) -> Value {
    let size = match &volume.storage {
        VolumeKind::Provisioned { maximum_bytes } => Some(size(*maximum_bytes)),
        VolumeKind::Docker {} => None,
    };
    json!({ "name": volume.name, "size": size, "shared_writes": volume.shared_writes })
}

tool_input! {
    pub(crate) struct VolumeAdd {
        /// Name for the new Volume, e.g. `data`. Must be unique in the Environment.
        #[schemars(with = "String", pattern(NODE_NAME))]
        name: VolumeName,
        /// Storage limit, e.g. `10GB` or `1TiB` (default 5 GB).
        #[serde(default)]
        #[schemars(pattern(SIZE), example = &"10GiB")]
        size: Option<String>,
        /// Service to mount the Volume into, e.g. `db`. Needs path.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        service: Option<ServiceName>,
        /// Where it mounts in that Service's containers, e.g. `/data`. Needs service.
        #[serde(default)]
        #[schemars(pattern("^/"))]
        path: Option<String>,
        @revision,
        @place(named),
    }
}

impl Tool for VolumeAdd {
    const NAME: &'static str = "volume_add";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Create a Volume";
    const DESCRIPTION: &'static str = "Create a Volume: persistent storage with a size limit, optionally mounted into one Service at a path. It is staged until the next deploy. Fails if the name is taken.";
    // The same input again finds the name taken and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "name": "data", "size": "10GiB", "service": "db", "path": "/var/lib/postgresql/data" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let mounts = match (self.service, self.path) {
            (Some(service), Some(path)) => vec![Mount { service, path }],
            (None, None) => Vec::new(),
            (Some(_), None) => return Err(Refusal::invalid("A mount needs a path", "path").into()),
            (None, Some(_)) => {
                return Err(Refusal::invalid("A mount needs a service", "service").into());
            }
        };
        let storage = match &self.size {
            Some(size) => bytes(size, "size")?,
            None => VolumeKind::provisioned_default(),
        };
        let create = CreateVolume {
            id: cx.mint(None)?,
            environment: scope.clone(),
            name: self.name,
            storage,
            mounts: mounts.clone(),
            shared_writes: false,
            storage_locked: false,
            restored_from: None,
        };
        let results = cx.batch(
            &scope,
            self.expect_revision,
            vec![BatchCommand::CreateVolume(create)],
        )?;
        let staged = written(results, |one| {
            if let Written::Volume(staged) = one {
                Some(staged)
            } else {
                None
            }
        })?;
        Ok(json!({
            "at": staged.environment.to_string(),
            "volume": volume(&staged.volume),
            "mounts": mounts.iter().map(|mount| json!({ "service": mount.service, "path": mount.path })).collect::<Vec<_>>(),
            "staged": true,
        }))
    }
}

tool_input! {
    pub(crate) struct VolumeUpdate {
        /// The Volume's current name, e.g. `data`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        volume: VolumeName,
        /// New storage limit, e.g. `20GB`. Fails once a Deployment has fixed the Volume's storage.
        #[serde(default)]
        #[schemars(pattern(SIZE), example = &"10GiB")]
        size: Option<String>,
        /// New name, e.g. `pgdata`. Must be unique in the Environment. Its mounts stay.
        #[serde(default)]
        #[schemars(with = "Option<String>", pattern(NODE_NAME))]
        to: Option<VolumeName>,
        /// Set true to let several containers write to the Volume. Applies right away; each Server learns it at the next deploy.
        #[serde(default)]
        shared_writes: Option<bool>,
        @revision,
        @place(named),
    }
}

impl Tool for VolumeUpdate {
    const NAME: &'static str = "volume_update";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Change a Volume";
    const DESCRIPTION: &'static str = "Change one thing about a Volume per call: its size limit, its name, or whether several containers may write to it. A new size or name is staged until the next deploy.";
    // A size or shared writes set again changes nothing; a rename sent again
    // finds the new name taken and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "volume": "data", "shared_writes": true })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let staged: VolumeStaged = match (self.size, self.to, self.shared_writes) {
            (Some(size), None, None) => {
                let storage = bytes(&size, "size")?;
                cx.expect(&scope, self.expect_revision)?;
                cx.write(&SetVolumeStorage {
                    environment: scope,
                    volume: self.volume,
                    storage,
                })?
            }
            (None, Some(name), None) => {
                cx.expect(&scope, self.expect_revision)?;
                cx.write(&RenameVolume {
                    environment: scope,
                    volume: self.volume,
                    name,
                })?
            }
            (None, None, Some(shared_writes)) => {
                cx.expect(&scope, self.expect_revision)?;
                cx.write(&SetVolumeSharedWrites {
                    environment: scope,
                    volume: self.volume,
                    shared_writes,
                })?
            }
            (None, None, None) => {
                return Err(Refusal::invalid("Give size, to or shared_writes", "size").into());
            }
            (Some(_) | None, Some(_) | None, Some(_) | None) => {
                return Err(Refusal::invalid(
                    "Change one of size, to or shared_writes per call",
                    "size",
                )
                .into());
            }
        };
        Ok(json!({
            "at": staged.environment.to_string(),
            "volume": volume(&staged.volume),
            "staged": !staged.staged.is_empty(),
        }))
    }
}

tool_input! {
    pub(crate) struct VolumeRemove {
        /// The Volume's name, e.g. `data`.
        #[schemars(with = "String", pattern(NODE_NAME))]
        volume: VolumeName,
        @revision,
        @place(named),
    }
}

impl Tool for VolumeRemove {
    const NAME: &'static str = "volume_remove";
    const STATUS: ToolId = ToolId::VolumeShow;
    const TITLE: &'static str = "Remove a Volume";
    const DESCRIPTION: &'static str = "Remove a Volume and its mounts in every Service. It is staged, so this call deletes no data. If the Volume is deployed, the next deploy deletes its data and asks a human to approve. Returns whether approval will be asked.";
    // A Change, not a Remove: the call only stages, and the Deploy that follows
    // deletes the data. Sent again, it finds no Volume by that name and is refused.
    const EFFECT: Effect = Effect::Change { idempotent: true };

    fn example() -> Value {
        json!({ "volume": "data" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(&self);
        let volumes = cx.one(&VolumesQuery {
            environment: scope.clone(),
        })?;
        cx.fresh(&volumes.environment, self.expect_revision)?;
        let staged = cx.write(&RemoveVolume {
            environment: scope.clone(),
            volume: self.volume,
        })?;
        let effects = cx.effects_after_write(scope)?;
        let asks_approval = effects_ask(
            &effects,
            &[DestructiveKind::DeletesVolume],
            staged.volume.id.as_str(),
        );
        Ok(json!({
            "at": staged.environment.to_string(),
            "removed": staged.volume.name,
            "asks_approval": asks_approval,
        }))
    }
}
