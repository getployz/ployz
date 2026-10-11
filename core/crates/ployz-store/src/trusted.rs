//! Evidence Cloud gathers outside the Store and passes in-process with a write. It is
//! never caller testimony: `read` and `write` over HTTPS carry none, only what Cloud found.

use ployz_core::{DockerVolumeId, DockerVolumeName, MachineId, RpcError};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::domain::DomainEvidence;
use crate::git::AuthorizedRepository;
use crate::id::ApprovalDigest;

/// What Cloud checked for one write. Empty unless Cloud supplies it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ConfigTrusted")]
pub struct Trusted {
    /// Repositories the calling Organization may read, with the branches Cloud saw.
    #[serde(default)]
    pub repositories: Vec<AuthorizedRepository>,
    /// What Cloud observed of the Organization's public domains and traffic.
    #[serde(default)]
    pub domains: DomainEvidence,
    /// Which Servers hold the Docker Volumes a Deploy would delete, when it deletes any.
    #[serde(default)]
    #[ts(optional)]
    pub volumes: Option<VolumeObservation>,
    /// How many Servers the Organization has, as Cloud counts them: a Deployment is
    /// admitted only when one could run it. None when the caller can't count them,
    /// such as the hidden local Store, which runs its Deployments itself.
    #[serde(default)]
    #[ts(optional)]
    pub servers: Option<u32>,
    /// Whether a human must approve a publication that destroys something, and what they
    /// approved.
    #[serde(default)]
    #[ts(as = "Option<Approval>", optional)]
    pub approval: Approval,
}

/// Whether Cloud asks a human before a publication destroys something, and which
/// destructive set a human approved. Cloud's evidence, never caller testimony.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// Nothing asks: the hidden local Store, or an Organization that doesn't require it.
    #[default]
    NotRequired,
    /// A publication that destroys something refuses until a human approves it.
    Required,
    /// Cloud holds a human's approval of exactly this `version:digest`.
    Approved(ApprovalDigest),
}

impl Trusted {
    /// Refuse to admit a Deployment no Server could run: `conflict`, as the
    /// Organization's state refuses it, naming `ployz server add`.
    pub(crate) fn runnable(&self) -> Result<(), RpcError> {
        if self.no_servers() {
            return Err(crate::error::conflict(
                NO_SERVERS,
                serde_json::json!({ "next": "ployz server add" }),
            ));
        }
        Ok(())
    }

    /// Cloud counted no Server: nothing runs, and nothing is left on one.
    pub(crate) const fn no_servers(&self) -> bool {
        matches!(self.servers, Some(0))
    }
}

/// Why nothing deploys in an Organization without Servers.
pub(crate) const NO_SERVERS: &str =
    "This Organization has no Server to run a Deployment: add one first";

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
