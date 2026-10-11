//! Tools on Volumes: create one, change one, remove one.

use ployz_core::config::VolumeKind;
use ployz_core::{ProvisionedVolumeMaximumBytes, ServiceName};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::machine::{Cx, Stop, Tool, at, stale, written};
use super::refusal::Refusal;
use crate::catalog::NODE_NAME;
use crate::{
    BatchCommand, CreateVolume, EnvironmentName, Mount, ProjectName, RemoveVolume, RenameVolume,
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

/// A Volume as a result shows it.
fn volume(volume: &VolumeSummary) -> Value {
    let size = match &volume.storage {
        VolumeKind::Provisioned { maximum_bytes } => Some(maximum_bytes.to_string()),
        VolumeKind::Docker {} => None,
    };
    json!({ "name": volume.name, "size": size, "shared_writes": volume.shared_writes })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeAdd {
    /// New Volume name, unique in the Environment.
    #[schemars(with = "String", pattern(NODE_NAME))]
    name: VolumeName,
    /// Storage limit. Default: 5GB.
    #[serde(default)]
    #[schemars(pattern(SIZE), example = &"10GiB")]
    size: Option<String>,
    /// Service to mount it into, with path.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    service: Option<ServiceName>,
    /// Absolute path in the Service's containers, with service.
    #[serde(default)]
    #[schemars(pattern("^/"))]
    path: Option<String>,
    /// Write only if the place is still at this revision. Omit unless you just read it.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeAdd {
    const NAME: &'static str = "volume_add";
    const DESCRIPTION: &'static str = "Create a Volume, persistent storage, and mount it into a Service at a path. Staged until the next Deploy.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "name": "data", "size": "10GiB", "service": "db", "path": "/var/lib/postgresql/data" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
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
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let create = CreateVolume {
            id: cx.mint(None)?,
            environment: scope.clone(),
            name: self.name,
            storage,
            mounts: mounts.clone(),
            shared_writes: false,
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
            "at": at(&staged.environment),
            "volume": volume(&staged.volume),
            "mounts": mounts.iter().map(|mount| json!({ "service": mount.service, "path": mount.path })).collect::<Vec<_>>(),
            "staged": true,
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeUpdate {
    #[schemars(with = "String", pattern(NODE_NAME))]
    volume: VolumeName,
    /// New storage limit. Only before its first Deploy.
    #[serde(default)]
    #[schemars(pattern(SIZE), example = &"10GiB")]
    size: Option<String>,
    /// New name.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    to: Option<VolumeName>,
    /// true lets more than one container write it. Applies at once.
    #[serde(default)]
    shared_writes: Option<bool>,
    /// Write only if the place is still at this revision. Omit unless you just read it.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeUpdate {
    const NAME: &'static str = "volume_update";
    const DESCRIPTION: &'static str = "Change one thing about a Volume per call: its size, its name, or whether several containers may write it.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "volume": "data", "shared_writes": true })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
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
            "at": at(&staged.environment),
            "volume": volume(&staged.volume),
            "staged": !staged.staged.is_empty(),
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VolumeRemove {
    #[schemars(with = "String", pattern(NODE_NAME))]
    volume: VolumeName,
    /// Write only if the place is still at this revision. Omit unless you just read it.
    #[serde(default)]
    expect_revision: Option<u64>,
    /// Project name. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    project: Option<ProjectName>,
    /// Environment name, e.g. staging. Omit for the one you are in.
    #[serde(default)]
    #[schemars(with = "Option<String>", pattern(NODE_NAME))]
    environment: Option<EnvironmentName>,
}

impl Tool for VolumeRemove {
    const NAME: &'static str = "volume_remove";
    const DESCRIPTION: &'static str = "Remove a Volume and unmount it everywhere. Staged: a deployed Volume's data is deleted at the next Publish or Deploy, which asks a human to approve.";
    const READ_ONLY: bool = false;

    fn example() -> Value {
        json!({ "volume": "data" })
    }

    fn plan(self, cx: &mut Cx<'_>) -> Result<Value, Stop> {
        let scope = cx.scope(self.project.as_ref(), self.environment.as_ref());
        let volumes = cx.one(&VolumesQuery {
            environment: scope.clone(),
        })?;
        if self
            .expect_revision
            .is_some_and(|expect| volumes.environment.revision.0 != expect)
        {
            return Err(stale(cx.call()));
        }
        let asks_approval = volumes
            .volumes
            .iter()
            .find(|listing| listing.volume.name == self.volume)
            .is_some_and(|listing| listing.deployed);
        let staged = cx.write(&RemoveVolume {
            environment: scope,
            volume: self.volume,
        })?;
        Ok(json!({
            "at": at(&staged.environment),
            "removed": staged.volume.name,
            "asks_approval": asks_approval,
        }))
    }
}
