//! Evidence Cloud gathers outside the Store and passes in-process with a write. It is
//! never caller testimony: `write` over HTTPS carries none, only what Cloud found.

use ployz_core::{DockerVolumeId, DockerVolumeName, MachineId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::git::AuthorizedRepository;

/// What Cloud checked for one write. Empty unless Cloud supplies it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ConfigTrusted")]
pub struct Trusted {
    /// Repositories the calling Organization may read, with the branches Cloud saw.
    #[serde(default)]
    pub repositories: Vec<AuthorizedRepository>,
    /// Which Servers hold the Docker Volumes a Deploy would delete, when it deletes any.
    #[serde(default)]
    pub volumes: Option<VolumeObservation>,
}

/// What the Servers answered when asked which of `sought` they hold. It is relative
/// to the observer: a Server that did not answer is named, never assumed empty.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumeObservation {
    /// The Docker Volumes asked about.
    pub sought: Vec<DockerVolumeName>,
    /// Each one found, on the Server holding it.
    pub held: Vec<DockerVolumeId>,
    /// Servers that did not answer, or were not asked: what they hold is unknown.
    pub unanswered: Vec<MachineId>,
}
