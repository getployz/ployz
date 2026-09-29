//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet. Cancelling stops one.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Command, replayable};
use crate::Actor;
use crate::deployment::{self, DeploymentSummary};
use crate::id::{DeploymentId, VolumeName};
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::trusted::Trusted;
use crate::{removal, review};

/// Deploy an Environment: all of it, or only some Services.
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
    let environment = scope::lock(tx, who, &admit.environment)?;
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
    let target = canonicalize_environment_intent(environment.working);
    // Only a full Deploy removes nodes, so only it can delete data.
    let removed = if admit.services.is_empty() {
        removal::removed(&review.head.applied, &target, &namespace)?
    } else {
        Vec::new()
    };
    let losses = removal::review(
        removed,
        trusted.volumes.as_ref(),
        &admit.accept_volume_loss,
        &review.view.version,
    )?;
    let frozen = deployment::freeze(
        id,
        &target,
        &review.head.applied,
        &admit.services,
        namespace,
        &losses,
    )?;
    deployment::admit(tx, who, &admit.id, id, saved, &admit.services, &frozen)
}
