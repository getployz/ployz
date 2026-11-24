//! The internal routes the daemon drives on the volume plugin, and the request types
//! SetVolumeCopy sends on its own, one route per ZFS effect.
//!
//! The daemon owns the transition table and the Container half; the plugin owns ZFS. Each
//! route is check-then-act over the datasets it finds, so a resend after a crash at any
//! point converges. Every route answers the copy as it is afterwards, the same shape
//! InspectVolumeCopy reports.

use ployz_core::{
    DockerVolumeName, ManagementAddress, SnapshotGuid, SnapshotName, VolumeGen, VolumeOrigin,
    WriterEpoch,
};
use serde::{Deserialize, Serialize};

/// The plugin's words, after a Volume's name, for a root it refused because a Container
/// that mounts the name expects other data. The daemon reads them back out of Docker's
/// refusal of a create.
pub const HELD_FOR_OTHER_DATA: &str = "is mounted by a Container approved for other data";

/// The plugin's words for a root it held back because Docker did not list the Containers
/// that mount the name in time. The daemon reads them back out of Docker's refusal of a
/// create.
pub const DOCKER_NOT_ANSWERING: &str = "Docker is not answering on this Machine";

/// The Pool's capacity and commitments, with no effect.
pub const STORAGE_INSPECT_ROUTE: &str = "Storage.Inspect";
/// The Pool made ready for Volumes.
pub const STORAGE_PREPARE_ROUTE: &str = "Storage.Prepare";
/// The Pool's roots parked read-only for a departing Machine.
pub const STORAGE_DEMOTE_ROUTE: &str = "Storage.Demote";
/// The departure snapshots taken.
pub const STORAGE_SEAL_ROUTE: &str = "Storage.Seal";
/// One Volume as the plugin finds it.
pub const VOLUME_INSPECT_ROUTE: &str = "Volume.Inspect";
/// A Volume created from a seed.
pub const VOLUME_SEED_ROUTE: &str = "Volume.Seed";
/// A Volume created from a seed a person's Copy step approved, carrying the step's birth.
pub const VOLUME_SEED_COPY_ROUTE: &str = "Volume.SeedCopy";
/// A Volume's bound set to a new size.
pub const VOLUME_SIZE_SET_ROUTE: &str = "Volume.Size.Set";
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
/// The dataset a send of `target` reads, with the snapshot's GUID and origin, observed in
/// one answer under the plugin's mutation, so no receive replaces the copy between them.
pub const SEND_POINT_ROUTE: &str = "Volume.SendPoint";

/// A seed the daemon admitted under a Copy step, and what the step binds it to.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SeedCopy {
    pub seed: ployz_core::SeedVolumeRequest,
    pub copy: CopyBinding,
}

/// What a Copy step holds a seed to on the plugin.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct CopyBinding {
    /// The origin of the step's birth: the slot records it, and the new Volume carries it
    /// once it lands. A slot or root of the new Volume that carries any other is not this
    /// copy's.
    pub birth: VolumeOrigin,
    /// The origin the source's point must carry, Legacy as its own value.
    pub source: VolumeOrigin,
    /// When the point was taken, as the source's snapshot records it to the second.
    pub taken_at: ployz_core::authority::Timestamp,
    /// The step's digest. The plugin reserves it before it makes the slot, and the slot
    /// carries it, so only that slot is this step's and a new Volume gone after the
    /// reservation was deleted.
    pub step: ployz_core::authority::StepDigest,
    /// The last instant on this Machine's clock a copy may start, or start over, under the
    /// step. The plugin reads it under its mutation, right before it does.
    pub closes: ployz_core::authority::Timestamp,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SendPointRequest {
    pub name: DockerVolumeName,
    pub target: SnapshotName,
}

/// What a send of one snapshot reads: the root or the slot's `fs`, the snapshot's GUID,
/// which the stream's BEGIN record must name, and its origin, `None` for Legacy.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SendPoint {
    pub dataset: String,
    pub guid: SnapshotGuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<VolumeOrigin>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CopyRequest {
    pub name: DockerVolumeName,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CutRequest {
    pub name: DockerVolumeName,
    pub target: SnapshotName,
    #[serde(default)]
    pub r#gen: VolumeGen,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ForgetRequest {
    pub name: DockerVolumeName,
    #[serde(default)]
    pub r#gen: VolumeGen,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnapshotRequest {
    pub name: DockerVolumeName,
    pub snapshot: SnapshotName,
    #[serde(default)]
    pub r#gen: VolumeGen,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WriterRequest {
    pub name: DockerVolumeName,
    pub at: SnapshotGuid,
    pub epoch: WriterEpoch,
    #[serde(default)]
    pub r#gen: VolumeGen,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequest {
    pub name: DockerVolumeName,
    pub from: ManagementAddress,
    pub target: SnapshotName,
    #[serde(default)]
    pub r#gen: VolumeGen,
}
