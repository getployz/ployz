//! Evidence Cloud gathers outside the Store and passes in-process with a write. It is
//! never caller testimony: `read` and `write` over HTTPS carry none, only what Cloud found.

use std::borrow::Cow;
use std::collections::BTreeSet;

use ployz_core::{
    DockerVolumeId, DockerVolumeName, MachineId, QualifiedService, RpcError, SealingPublicKey,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::domain::DomainEvidence;
use crate::git::AuthorizedRepository;
use crate::id::{ApprovalDigest, EnvironmentName, GithubUserId};
use crate::permission::Permission;
use crate::storage::Tx;

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
    /// The Servers that didn't answer Cloud just before a removal's admission: a
    /// removal that failed goes again once one of them answers. None when Cloud
    /// couldn't look, which says nothing of who answered.
    #[serde(default)]
    #[ts(optional)]
    pub unanswered: Option<Vec<MachineId>>,
    /// The Services the Servers run, as Cloud observed them just before a full
    /// Deploy's admission: what its prune removes, by name. None when Cloud didn't
    /// look, so nothing names what a prune removes.
    #[serde(default)]
    #[ts(optional)]
    pub running: Option<RunningObservation>,
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
    /// Whether the caller's Member role can protect Environments, though its token's grant may
    /// leave `environments:protect` out: an owner's or admin's Deploy token. A write that
    /// ships into a Protected Environment doesn't ask another protector for it. Every fence
    /// that keys on the permission still reads [`Self::permissions`].
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub protector: bool,
    /// The Environments the caller's token may write, by name: a write to any other is
    /// refused. None when its grant names none, so it may write any it has permission to.
    #[serde(default)]
    #[ts(optional)]
    pub environments: Option<BTreeSet<EnvironmentName>>,
    /// The user Cloud authenticated, by their immutable id, a token by its owner's: what
    /// makes a Deployment someone's own. A name may repeat; this never does. None for
    /// Cloud's own automation and the hidden local Store.
    #[serde(default)]
    #[ts(optional)]
    pub user: Option<String>,
    /// Whether a human must approve a publication that destroys something, and what they
    /// approved.
    #[serde(default)]
    #[ts(as = "Option<Approval>", optional)]
    pub approval: Approval,
    /// Which Deploys `approval` gates. Publish asks only when it destroys something.
    #[serde(default)]
    #[ts(as = "Option<Asks>", optional)]
    pub asks: Asks,
    /// Whether the write came over HTTPS from the CLI, which seals its own secrets:
    /// a plain-text secret from one that doesn't is refused with the update to run.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub from_cli: bool,
    /// The Cluster's Sealing public key, as Cloud read it: what the Store seals a
    /// generated secret to. None when the Cluster has no Server yet, which refuses a
    /// generated secret.
    #[serde(default)]
    #[ts(optional)]
    pub sealing: Option<SealingRecipient>,
}

/// The public key a value sealed in the Store is sealed to, which only the Cluster's
/// Machines open.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SealingRecipient {
    pub public_key: SealingPublicKey,
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

    /// This evidence with the Servers a failed Volume check couldn't check named from
    /// `who`'s Deployments: the Server that answered with an error, else those the check
    /// meant to ask. A Server no Deployment ran on keeps its ID.
    pub(crate) fn naming(
        &self,
        tx: &mut dyn Tx,
        who: &crate::Actor,
    ) -> Result<Cow<'_, Self>, RpcError> {
        let Some((observed, failure)) = self
            .volumes
            .as_ref()
            .and_then(|observed| Some((observed, observed.failure.as_ref()?)))
        else {
            return Ok(Cow::Borrowed(self));
        };
        let deployed = crate::deployment::servers(tx, &who.organization)?;
        let named = |server: &MachineId| {
            deployed
                .get(server)
                .cloned()
                .unwrap_or_else(|| server.to_string())
        };
        let servers = match &failure.stage {
            ObservationStage::Answered { server } => server.iter().map(named).collect(),
            ObservationStage::Unreachable => observed.unanswered.iter().map(named).collect(),
        };
        let mut trusted = self.clone();
        if let Some(failure) = trusted
            .volumes
            .as_mut()
            .and_then(|observed| observed.failure.as_mut())
        {
            failure.servers = servers;
        }
        Ok(Cow::Owned(trusted))
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

    /// Whether the caller holds `permission`, as [`Self::admit`] checks it.
    pub(crate) fn holds(&self, permission: Permission) -> bool {
        self.permissions
            .as_ref()
            .is_none_or(|held| held.contains(&permission))
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

    pub(crate) fn refuse_written_outside_environments(
        &self,
        tx: &mut dyn Tx,
        written: &crate::Written,
    ) -> Result<(), RpcError> {
        let Some(allowed) = &self.environments else {
            return Ok(());
        };
        let mut names = Vec::new();
        wrote(tx, written, &mut names)?;
        if !names.is_empty() && names.iter().all(|name| allowed.contains(name)) {
            return Ok(());
        }
        Err(outside(allowed))
    }

    /// Refuse a write to Environment `name` outside this token's Environments before
    /// it asks a human to approve anything there.
    pub(crate) fn writes(&self, name: &EnvironmentName) -> Result<(), RpcError> {
        match &self.environments {
            Some(allowed) if !allowed.contains(name) => Err(outside(allowed)),
            _ => Ok(()),
        }
    }
}

fn outside(allowed: &BTreeSet<EnvironmentName>) -> RpcError {
    crate::error::forbidden(
        format!("This token can deploy only to {}.", listed(allowed)),
        serde_json::json!({ "environments": allowed }),
    )
}

/// Every Environment `written` wrote, by name: each command's in a Batch, the Branch a
/// Sync closed as well as the one it staged into, and the one a protection write
/// changed, which writes no config. A closed Branch may be gone already, so a Sync
/// names both from what it answered.
fn wrote(
    tx: &mut dyn Tx,
    written: &crate::Written,
    names: &mut Vec<EnvironmentName>,
) -> Result<(), RpcError> {
    if let crate::Written::Batch(batched) = written {
        for each in &batched.results {
            wrote(tx, each, names)?;
        }
    } else if let crate::Written::Synced(synced) = written {
        names.push(synced.into.name.clone());
        if synced.closing {
            names.push(synced.from.name.clone());
        }
    } else if let crate::Written::Protected(protected) = written {
        names.push(
            crate::scope::load_by_id(tx, &protected.environment)?
                .summary
                .name,
        );
    } else if let Some(id) = written.environment() {
        names.push(crate::scope::load_by_id(tx, id)?.summary.name);
    }
    Ok(())
}

fn listed(names: &BTreeSet<EnvironmentName>) -> String {
    let names: Vec<&str> = names.iter().map(EnvironmentName::as_str).collect();
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => names.concat(),
    }
}

/// Why nothing deploys in an Organization without Servers, worded as Cloud's
/// refusal of a runtime read without them.
pub(crate) const NO_SERVERS: &str =
    "This Organization has no servers yet, so nothing deployed. Add one, then deploy again.";

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
    /// Why the observer couldn't check the Servers at all: then nothing was checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure: Option<ObservationFailure>,
}

impl VolumeObservation {
    /// The observation of a check of `sought` that failed: `unanswered` names each
    /// Server it meant to ask, when the observer knows them.
    #[must_use]
    pub fn failed(
        sought: Vec<DockerVolumeName>,
        unanswered: Vec<MachineId>,
        failure: ObservationFailure,
    ) -> Self {
        Self {
            sought,
            held: Vec::new(),
            unanswered,
            failure: Some(failure),
        }
    }
}

/// The Services running on the Servers that answered, by Namespace.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RunningObservation {
    /// Each Service with a Container on a Server that answered.
    pub services: Vec<QualifiedService>,
    /// Servers that did not answer: what they run is unknown.
    pub unanswered: Vec<MachineId>,
    /// Why the observer couldn't check the Servers at all: then nothing was checked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure: Option<ObservationFailure>,
}

/// Why an observer couldn't check the Servers, and where the check stopped.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ObservationFailure {
    pub stage: ObservationStage,
    /// The observer's sentence for what failed.
    pub reason: String,
    /// Each source below `reason`, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub cause: Vec<String>,
    /// The endpoint the check dialed, as the user gave it, when the observer doesn't
    /// know which Server it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub endpoint: Option<String>,
    /// The Servers it couldn't check, by name: the Store fills them from its
    /// Deployments before it reviews ([`Trusted::naming`]).
    #[serde(skip)]
    #[ts(skip)]
    pub(crate) servers: Vec<String>,
}

/// Where a Volume check stopped.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservationStage {
    /// No Server could be reached.
    Unreachable,
    /// A Server answered the check with an error: `server`, when the observer knows
    /// which.
    Answered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        server: Option<MachineId>,
    },
}

impl ObservationFailure {
    /// `error`, from reaching the Servers.
    #[must_use]
    pub fn unreachable(error: RpcError) -> Self {
        Self::at(ObservationStage::Unreachable, error)
    }

    /// `error`, the answer of `server` (when known) to the check.
    #[must_use]
    pub fn answered(server: Option<MachineId>, error: RpcError) -> Self {
        Self::at(ObservationStage::Answered { server }, error)
    }

    /// This failure, from dialing `endpoint`.
    #[must_use]
    pub fn at_endpoint(self, endpoint: Option<String>) -> Self {
        Self { endpoint, ..self }
    }

    fn at(stage: ObservationStage, error: RpcError) -> Self {
        Self {
            stage,
            reason: error.message,
            cause: error.cause,
            endpoint: None,
            servers: Vec::new(),
        }
    }
}
