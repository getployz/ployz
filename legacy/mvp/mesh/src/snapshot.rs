use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use mvp_bus::IslandId;
use mvp_identity::NodeId;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{MeshError, MeshResult, WireGuardOverlayIp, WireGuardPeer, WireGuardPeerPlan};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardAppliedSnapshot {
    pub island: String,
    pub local_node_id: NodeId,
    pub local_overlay_ip: WireGuardOverlayIp,
    pub revision: u64,
    pub peers: Vec<WireGuardPeer>,
}

impl WireGuardAppliedSnapshot {
    #[must_use]
    pub fn from_plan(island: IslandId, plan: WireGuardPeerPlan) -> Self {
        Self {
            island: island.as_str().to_string(),
            local_node_id: plan.local_node_id,
            local_overlay_ip: plan.local_overlay_ip,
            revision: plan.revision,
            peers: plan.peers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardSnapshotPaths {
    pub applied: PathBuf,
}

impl WireGuardSnapshotPaths {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            applied: root.join("wireguard.applied.snapshot"),
        }
    }
}

pub fn write_applied_snapshot(
    paths: &WireGuardSnapshotPaths,
    snapshot: &WireGuardAppliedSnapshot,
) -> MeshResult<()> {
    if let Some(parent) = paths.applied.parent() {
        fs::create_dir_all(parent).map_err(|error| snapshot_io("create parent", parent, error))?;
    }
    reject_symlink(&paths.applied)?;
    let bytes =
        serde_json::to_vec_pretty(snapshot).map_err(|error| MeshError::InvalidSnapshot {
            path: paths.applied.display().to_string(),
            message: error.to_string(),
        })?;
    let parent = paths.applied.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp =
        NamedTempFile::new_in(parent).map_err(|error| snapshot_io("create temp", parent, error))?;
    tmp.write_all(&bytes)
        .map_err(|error| snapshot_io("write temp", tmp.path(), error))?;
    tmp.as_file_mut()
        .sync_all()
        .map_err(|error| snapshot_io("sync temp", tmp.path(), error))?;
    reject_symlink(&paths.applied)?;
    tmp.persist(&paths.applied)
        .map(|_file| ())
        .map_err(|error| snapshot_io("persist", &paths.applied, error.error))
}

pub fn load_applied_snapshot(
    paths: &WireGuardSnapshotPaths,
    expected_island: &IslandId,
) -> MeshResult<WireGuardAppliedSnapshot> {
    reject_symlink(&paths.applied)?;
    let bytes =
        fs::read(&paths.applied).map_err(|error| snapshot_io("read", &paths.applied, error))?;
    let snapshot = serde_json::from_slice::<WireGuardAppliedSnapshot>(&bytes).map_err(|error| {
        MeshError::InvalidSnapshot {
            path: paths.applied.display().to_string(),
            message: error.to_string(),
        }
    })?;
    if snapshot.island != expected_island.as_str() {
        return Err(MeshError::InvalidSnapshot {
            path: paths.applied.display().to_string(),
            message: format!(
                "snapshot island {} did not match expected {}",
                snapshot.island,
                expected_island.as_str()
            ),
        });
    }
    Ok(snapshot)
}

fn reject_symlink(path: &Path) -> MeshResult<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(MeshError::InvalidSnapshot {
            path: path.display().to_string(),
            message: "symlink target rejected".to_string(),
        });
    }
    Ok(())
}

fn snapshot_io(operation: &'static str, path: &Path, error: std::io::Error) -> MeshError {
    MeshError::SnapshotIo {
        operation,
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use mvp_bus::IslandId;
    use mvp_identity::NodeId;
    use mvp_projection::{NodeProjection, ProjectionState};
    use tempfile::tempdir;

    use crate::{WireGuardAppliedSnapshot, WireGuardSnapshotPaths, plan_full_mesh};

    use super::{load_applied_snapshot, write_applied_snapshot};

    #[test]
    fn snapshot_round_trips_and_checks_island() {
        let temp = tempdir().expect("tempdir");
        let paths = WireGuardSnapshotPaths::new(temp.path());
        let island = IslandId::new("prod");
        let plan = plan_full_mesh(&projection(), &NodeId::new("node-1"), 1).expect("plan");
        let snapshot = WireGuardAppliedSnapshot::from_plan(island.clone(), plan);

        write_applied_snapshot(&paths, &snapshot).expect("write snapshot");

        assert_eq!(
            load_applied_snapshot(&paths, &island).expect("load snapshot"),
            snapshot
        );
        assert!(load_applied_snapshot(&paths, &IslandId::new("dev")).is_err());
    }

    #[test]
    fn snapshot_rejects_invalid_json() {
        let temp = tempdir().expect("tempdir");
        let paths = WireGuardSnapshotPaths::new(temp.path());
        fs::write(&paths.applied, b"not-json").expect("write invalid snapshot");

        assert!(load_applied_snapshot(&paths, &IslandId::new("prod")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_symlink_applied_path() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let paths = WireGuardSnapshotPaths::new(temp.path());
        let target = temp.path().join("outside.snapshot");
        symlink(&target, &paths.applied).expect("create symlink");
        let plan = plan_full_mesh(&projection(), &NodeId::new("node-1"), 1).expect("plan");
        let snapshot = WireGuardAppliedSnapshot::from_plan(IslandId::new("prod"), plan);

        assert!(write_applied_snapshot(&paths, &snapshot).is_err());
        assert!(!target.exists());
    }

    fn projection() -> ProjectionState {
        let mut state = ProjectionState::for_island(IslandId::new("prod"));
        for index in 0..2 {
            let node_id = NodeId::new(format!("node-{index}"));
            state.nodes.insert(
                node_id.clone(),
                NodeProjection {
                    node_id,
                    epoch: 1,
                    overlay_ip: format!("fd00::{index:x}"),
                    iroh_endpoint_id: format!("iroh-node-{index}"),
                    wg_public_key: format!("wg-node-{index}"),
                },
            );
        }
        state
    }
}
