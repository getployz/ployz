use std::fs;
use std::path::{Path, PathBuf};

use mvp_bus::IslandId;
use mvp_projection::NodeId;
use serde::{Deserialize, Serialize};

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

    #[must_use]
    pub fn has_peer(&self, node_id: &NodeId) -> bool {
        self.peers.iter().any(|peer| &peer.node_id == node_id)
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
    let tmp = paths.applied.with_extension("snapshot.tmp");
    reject_symlink(&tmp)?;
    let bytes =
        serde_json::to_vec_pretty(snapshot).map_err(|error| MeshError::InvalidSnapshot {
            path: paths.applied.display().to_string(),
            message: error.to_string(),
        })?;
    fs::write(&tmp, bytes).map_err(|error| snapshot_io("write", &tmp, error))?;
    fs::rename(&tmp, &paths.applied).map_err(|error| snapshot_io("rename", &paths.applied, error))
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
    use mvp_bus::IslandId;
    use mvp_projection::{NodeId, NodeProjection, ProjectionState};
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
