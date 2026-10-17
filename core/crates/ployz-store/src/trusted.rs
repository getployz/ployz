//! Evidence Cloud gathers outside the Store and passes in-process with a write. It is
//! never caller testimony: `read` and `write` over HTTPS carry none, only what Cloud found.

use std::borrow::Cow;
use std::collections::BTreeSet;

use ployz_core::{DockerVolumeId, DockerVolumeName, MachineId, RpcError};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::domain::DomainEvidence;
use crate::git::AuthorizedRepository;
use crate::id::{ApprovalDigest, GithubUserId};
use crate::permission::Permission;

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
    /// The GitHub accounts of the Organization's members, as Cloud linked them: a pull
    /// request deploys without approval only when one of them wrote it. None when Cloud
    /// didn't look, which trusts no one.
    #[serde(default)]
    #[ts(optional)]
    pub members: Option<Vec<GithubUserId>>,
    /// The permissions the caller holds, as Cloud grants them by role: a command
    /// refuses unless the one it names is among them. None when no one checked, which
    /// is only the hidden local Store, whose caller owns everything in it, and Cloud's
    /// own automation. A named caller with none sent holds none ([`Self::held_by`]).
    #[serde(default)]
    #[ts(optional)]
    pub permissions: Option<BTreeSet<Permission>>,
    /// Whether a human must approve a publication that destroys something, and what they
    /// approved.
    #[serde(default)]
    #[ts(as = "Option<Approval>", optional)]
    pub approval: Approval,
    /// Which Deploys `approval` gates. Publish asks only when it destroys something.
    #[serde(default)]
    #[ts(as = "Option<Asks>", optional)]
    pub asks: Asks,
    /// Whether the caller may change a Protected Environment's repository or deploy
    /// branch, flip its protection, or remove its Project, as Cloud found their role.
    /// The Store judges it against `protected` read in the same transaction, so a
    /// protect that lands first still fences the write. The Store's own automation
    /// always may. P7's `needs` replaces this flag.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub may_protect: bool,
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

/// Which Deploys wait on a human when `approval` is required. Cloud's evidence of who
/// called, never caller testimony.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Asks {
    /// A Deploy asks only when it destroys something new.
    #[default]
    Destroys,
    /// Every Deploy asks, even one that destroys nothing or ships already-Saved State:
    /// an agent asked for it.
    EveryDeploy,
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

    /// This evidence as `who` is checked against it. A named caller with no permission
    /// set holds none: Cloud sends every user's set, so a missing one is a defect, not
    /// a grant. Only the Store's own automation and the local Store go unchecked.
    pub(crate) fn held_by(&self, who: &crate::Actor) -> Cow<'_, Self> {
        if who.principal.is_some() && self.permissions.is_none() {
            Cow::Owned(Self {
                permissions: Some(BTreeSet::new()),
                ..self.clone()
            })
        } else {
            Cow::Borrowed(self)
        }
    }

    pub(crate) fn admit(&self, permission: Permission) -> Result<(), RpcError> {
        match &self.permissions {
            Some(held) if !held.contains(&permission) => Err(crate::error::forbidden(
                format!("Needs {permission}"),
                serde_json::json!({ "permission": permission }),
            )),
            _ => Ok(()),
        }
    }
}

/// Why nothing deploys in an Organization without Servers, worded as Cloud's
/// refusal of a runtime read without them.
pub(crate) const NO_SERVERS: &str = "This Organization has no servers yet, so nothing deployed. Add one from a terminal, then deploy again.";

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
