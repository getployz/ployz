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

/// Deploy an Environment: all of it, or only some Services. Or, with `retry`,
/// ship again exactly what an ended Deployment froze.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Admit {
    pub id: DeploymentId,
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Deploy only these Services; none deploys every Service.
    #[serde(default)]
    pub services: Vec<ServiceName>,
    /// Refuse with `conflict` unless this is still the latest `diff` version.
    #[serde(default)]
    pub version: Option<String>,
    /// A new upload for Services without a source of their own; none keeps the
    /// Environment's latest.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub upload: Option<UploadedSource>,
    /// Retry this failed, unknown or cancelled Deployment: its Saved revision,
    /// targets and Namespace, whatever was saved since. It names the Environment,
    /// so `environment`, `services`, `version` and `upload` stay empty.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub retry: Option<DeploymentId>,
    /// Remove the Environment from the Servers: ship it empty, deleting its deployed
    /// Volumes, without touching Working or Saved State. `services`, `version`,
    /// `upload` and `retry` stay empty. `RemoveEnvironment` then deletes it.
    #[serde(default)]
    pub remove: bool,
    /// Deployed Volumes whose data this Deploy may delete, by name. A Deploy that
    /// deletes data refuses with `confirmation_required` unless it names each one.
    #[serde(default)]
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
    if let Some(source) = &admit.retry {
        if admit.environment != EnvironmentRef::default()
            || admit.remove
            || !admit.services.is_empty()
            || admit.version.is_some()
            || admit.upload.is_some()
            || !admit.accept_volume_loss.is_empty()
        {
            return Err(error::invalid(
                "A retry ships what its Deployment froze: leave environment, remove, services, \
                 version, upload and accept_volume_loss empty",
                json!({}),
            ));
        }
        // The retry deletes exactly the Docker Volumes its source's review accepted,
        // which the copied target nodes carry: nothing new, so no new review.
        return deployment::retry(tx, who, &admit.id, source);
    }
    if admit.remove {
        return removal(tx, who, admit, trusted);
    }
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
    let (saved, _) = review::publish(
        tx,
        who,
        id,
        environment.working.clone(),
        review.saved.as_ref(),
    )?;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let saved_intent = canonicalize_environment_intent(environment.working);
    // Only a full Deploy removes nodes, so only it can delete data.
    let removed = if admit.services.is_empty() {
        removal::removed(&review.head.applied, &saved_intent, &namespace)?
    } else {
        Vec::new()
    };
    let losses = removal::review(
        removed,
        trusted.volumes.as_ref(),
        &admit.accept_volume_loss,
        &review.view.version,
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
    let mut admit = admit.clone();
    if let Some(upload) = &mut admit.upload {
        upload.uploader.clone_from(&trusted.uploader);
    }
    deployment::admit(tx, who, &admit, id, saved, &frozen)
}

/// Queue the Deployment that removes an Environment from the Servers: the empty
/// Environment against everything Applied State holds, under the same destructive
/// review as any Deploy. A running Deployment must end first, so the removal's
/// targets are everything that ran.
fn removal(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Admit,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    if !admit.services.is_empty() || admit.version.is_some() || admit.upload.is_some() {
        return Err(error::invalid(
            "A removal ships nothing: leave services, version and upload empty",
            json!({}),
        ));
    }
    let environment = scope::lock(tx, who, &admit.environment)?;
    crate::teardown::guard(tx, &environment)?;
    let id = &environment.summary.id;
    let history = deployment::history(tx, id, i64::MAX)?;
    if let Some(running) = history.iter().find(|deployment| {
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
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let empty = review::empty(&environment.working);
    let removed = removal::removed(&review.head.applied, &empty, &namespace)?;
    let losses = removal::review(
        removed,
        trusted.volumes.as_ref(),
        &admit.accept_volume_loss,
        &review.view.version,
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
    deployment::admit(tx, who, admit, id, deployment::NOTHING, &frozen)
}
