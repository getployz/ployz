//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet; retrying queues what
//! an ended Deployment froze. Starting hands a queued one to a runner again, and
//! cancelling stops one.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{Command, replayable};
use crate::Actor;
use crate::deployment::{self, DeploymentSummary};
use crate::error;
use crate::id::DeploymentId;
use crate::review;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;

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
    /// Retry this failed, unknown or cancelled Deployment: its Saved revision,
    /// targets and Namespace, whatever was saved since. It names the Environment,
    /// so `environment`, `services` and `version` stay empty.
    #[serde(default)]
    pub retry: Option<DeploymentId>,
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
) -> Result<DeploymentSummary, RpcError> {
    let command = Command::Admit(admit.clone());
    replayable(tx, who, &command, |tx| admitted(tx, who, admit))
}

fn admitted(tx: &mut dyn Tx, who: &Actor, admit: &Admit) -> Result<DeploymentSummary, RpcError> {
    if let Some(source) = &admit.retry {
        if admit.environment != EnvironmentRef::default()
            || !admit.services.is_empty()
            || admit.version.is_some()
        {
            return Err(error::invalid(
                "A retry ships what its Deployment froze: leave environment, services and \
                 version empty",
                json!({}),
            ));
        }
        return deployment::retry(tx, who, &admit.id, source);
    }
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
    let frozen = deployment::freeze(
        id,
        &canonicalize_environment_intent(environment.working),
        &review.head.applied,
        &admit.services,
        namespace,
    )?;
    deployment::admit(tx, who, &admit.id, id, saved, &admit.services, &frozen)
}
