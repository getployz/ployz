//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet; retrying queues what
//! an ended Deployment froze. Starting hands a queued one to a runner again, and
//! cancelling stops one.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{Command, replayable};
use crate::deployment::{self, DeploymentStatus, DeploymentSummary, UploadedSource};
use crate::domain;
use crate::error;
use crate::id::{DeploymentId, VolumeName};
use crate::registry;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, Trusted};
use crate::{removal, review};

/// Queue a Deployment: a Deploy of Saved State, a retry of an ended Deployment, or
/// the removal of an Environment from the Servers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "admit", rename_all = "snake_case")]
pub enum Admit {
    /// Publish Working State if needed, then deploy all of it or some Services.
    Deploy(Deploy),
    /// Ship again exactly what an ended Deployment froze.
    Retry(Retry),
    /// Take an Environment off the Servers.
    Remove(Removal),
}

impl Admit {
    /// The new Deployment's ID.
    #[must_use]
    pub const fn id(&self) -> &DeploymentId {
        match self {
            Self::Deploy(deploy) => &deploy.id,
            Self::Retry(retry) => &retry.id,
            Self::Remove(removal) => &removal.id,
        }
    }
}

/// Deploy an Environment: all of it, or only some Services.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Deploy {
    pub id: DeploymentId,
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Deploy only these Services; none deploys every Service.
    #[serde(default)]
    #[ts(as = "Option<Vec<ServiceName>>", optional)]
    pub services: Vec<ServiceName>,
    /// Refuse with `conflict` unless this is still the latest `diff` version, or the
    /// version a refusal to delete data handed back.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
    /// A new upload for Services without a source of their own; none keeps the
    /// Environment's latest.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub upload: Option<UploadedSource>,
    /// Deployed Volumes whose data this Deploy may delete, by name. A Deploy that
    /// deletes data, or publishes a removal that will, refuses with
    /// `confirmation_required` unless it names each one and passes the `version`
    /// that refusal handed back.
    #[serde(default)]
    #[ts(as = "Option<Vec<VolumeName>>", optional)]
    pub accept_volume_loss: Vec<VolumeName>,
}

/// Retry a failed, unknown or cancelled Deployment: its Saved revision, targets,
/// Namespace, credentials and upload, whatever was saved since.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub id: DeploymentId,
    /// The Deployment it ships again.
    pub deployment: DeploymentId,
}

/// Remove an Environment from the Servers: ship it empty, deleting its deployed
/// Volumes, without touching Working or Saved State. `RemoveEnvironment` then
/// deletes it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Removal {
    pub id: DeploymentId,
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// As [`Deploy::version`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
    /// As [`Deploy::accept_volume_loss`].
    #[serde(default)]
    #[ts(as = "Option<Vec<VolumeName>>", optional)]
    pub accept_volume_loss: Vec<VolumeName>,
}

/// Cancel a Deployment: a queued one never runs, and a running one stops.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub deployment: DeploymentId,
}

/// Hand a queued Deployment to a runner now: the dashboard's "Deploy now". It
/// changes nothing stored; Cloud dispatches its worker again after it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub deployment: DeploymentId,
}

pub(crate) fn start(
    tx: &mut dyn Tx,
    who: &Actor,
    start: &Start,
) -> Result<DeploymentSummary, RpcError> {
    deployment::start(tx, who, &start.deployment)
}

pub(crate) fn cancel(
    tx: &mut dyn Tx,
    who: &Actor,
    cancel: &Cancel,
) -> Result<DeploymentSummary, RpcError> {
    deployment::cancel(tx, who, &cancel.deployment)
}

pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Admit,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let command = Command::Admit(admit.clone());
    replayable(tx, who, &command, |tx| admitted(tx, who, admit, trusted))
}

fn admitted(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Admit,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    trusted.runnable()?;
    match admit {
        // The retry deletes exactly the Docker Volumes its source's review accepted,
        // which the copied target nodes carry: nothing new, so no new review.
        Admit::Retry(retry) => deployment::retry(tx, who, &retry.id, &retry.deployment),
        Admit::Remove(removal) => self::removal(tx, who, removal, trusted),
        Admit::Deploy(deploy) => self::deploy(tx, who, deploy, trusted),
    }
}

fn deploy(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Deploy,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let environment = scope::lock(tx, who, &admit.environment)?;
    // Cloud reserves the Cluster Domain before admitting a generated domain.
    let cluster_domain = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    if cluster_domain.is_none() && domain::has_generated(&environment.working) {
        return Err(RpcError {
            code: RpcErrorCode::Unsupported,
            message: "Generated domains deploy only through Ployz Cloud, which holds the \
                      Cluster Domain"
                .into(),
            details: json!({}),
        });
    }
    let review = review::review(tx, &environment)?;
    review::check(&review, admit.version.as_deref())?;
    let id = &environment.summary.id;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let saved_intent = canonicalize_environment_intent(environment.working.clone());
    // A full Deploy removes what Saved State dropped; publishing puts a removal in
    // Saved State. Either runs the destructive review; only the first deletes.
    let publishes = review
        .saved
        .as_ref()
        .is_none_or(|saved| saved.intent != saved_intent);
    let removed = if admit.services.is_empty() || publishes {
        removal::removed(&review.head.applied, &saved_intent, &namespace)?
    } else {
        Vec::new()
    };
    let losses = removal::review(
        who,
        id,
        (&review.view.version, admit.version.as_deref()),
        removed,
        trusted.volumes.as_ref(),
        &admit.accept_volume_loss,
    )?;
    let losses = if admit.services.is_empty() {
        losses
    } else {
        Vec::new()
    };
    let (saved, _) = review::publish(
        tx,
        who,
        id,
        environment.working.clone(),
        review.saved.as_ref(),
    )?;
    let mut frozen = deployment::freeze(
        id,
        &saved_intent,
        &review.head.applied,
        &admit.services,
        namespace,
        cluster_domain,
        &losses,
    )?;
    frozen.credentials = registry::freeze(tx, id, &saved_intent, &frozen)?;
    // Only Cloud's authentication names an uploader, never the caller.
    let upload = admit.upload.clone().map(|upload| UploadedSource {
        uploader: who.principal.clone(),
        ..upload
    });
    deployment::admit(
        tx,
        who,
        (&admit.id, &admit.services, upload),
        id,
        saved,
        &frozen,
    )
}

/// Queue the Deployment that removes an Environment from the Servers: the empty
/// Environment against everything Applied State holds, under the same destructive
/// review as any Deploy. A running Deployment must end first, so the removal's
/// targets are everything that ran.
fn removal(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Removal,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    let environment = scope::lock(tx, who, &admit.environment)?;
    crate::teardown::guard_removal(tx, &environment)?;
    let id = &environment.summary.id;
    if let Some(running) = deployment::in_flight(tx, id)?.filter(|deployment| {
        matches!(
            deployment.status,
            DeploymentStatus::Running | DeploymentStatus::Cancelling
        )
    }) {
        return Err(error::conflict(
            format!(
                "Deployment #{} is running: wait for it or cancel it before removing {}",
                running.number, environment.summary.name
            ),
            json!({ "deployment": running.id }),
        ));
    }
    let review = review::review(tx, &environment)?;
    review::check(&review, admit.version.as_deref())?;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let empty = review::empty(&environment.working.environment_slug);
    let removed = removal::removed(&review.head.applied, &empty, &namespace)?;
    let losses = removal::review(
        who,
        id,
        (&review.view.version, admit.version.as_deref()),
        removed,
        trusted.volumes.as_ref(),
        &admit.accept_volume_loss,
    )?;
    let frozen = deployment::freeze(
        id,
        &empty,
        &review.head.applied,
        &[],
        namespace,
        None,
        &losses,
    )?;
    deployment::admit(
        tx,
        who,
        (&admit.id, &[], None),
        id,
        deployment::NOTHING,
        &frozen,
    )
}
