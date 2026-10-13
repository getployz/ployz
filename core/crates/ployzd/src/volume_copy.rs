//! The internal routes SetVolumeCopy drives on the volume plugin, one per ZFS effect.
//!
//! The daemon owns the transition table and the Container half; the plugin owns ZFS. Each
//! route is check-then-act over the datasets it finds, so a resend after a crash at any
//! point converges. Every route answers the copy as it is afterwards, the same shape
//! InspectVolumeCopy reports.

use ployz_core::{DockerVolumeName, ManagementAddress, SnapshotGuid, SnapshotName, WriterEpoch};
use serde::{Deserialize, Serialize};

/// The copy as it is now, with no effect.
pub const INSPECT_ROUTE: &str = "Volume.Copy.Inspect";
/// The cut: the writer root becomes the slot's read-only copy at `target`.
pub const CUT_ROUTE: &str = "Volume.Copy.Cut";
/// A run snapshot on the writer root, pruned to the newest two.
pub const SNAPSHOT_ROUTE: &str = "Volume.Copy.Snapshot";
/// Every run snapshot dropped from the writer root.
pub const FORGET_ROUTE: &str = "Volume.Copy.Forget";
/// A writable root at `at` numbered `epoch`, promoted from the slot when the root is absent.
pub const WRITER_ROUTE: &str = "Volume.Copy.Writer";
/// The pull of `target` from `from` into the slot, created when absent.
pub const PULL_ROUTE: &str = "Volume.Copy.Pull";
/// The slot destroyed with its copy.
pub const DESTROY_ROUTE: &str = "Volume.Copy.Destroy";

/// The path on the writer Machine's send port that answers a Volume's bound in bytes.
pub const BOUND_PATH_PREFIX: &str = "/volume-bound/";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CopyRequest {
    pub name: DockerVolumeName,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CutRequest {
    pub name: DockerVolumeName,
    pub target: SnapshotName,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnapshotRequest {
    pub name: DockerVolumeName,
    pub snapshot: SnapshotName,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WriterRequest {
    pub name: DockerVolumeName,
    pub at: SnapshotGuid,
    pub epoch: WriterEpoch,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequest {
    pub name: DockerVolumeName,
    pub from: ManagementAddress,
    pub target: SnapshotName,
}
