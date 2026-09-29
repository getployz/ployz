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
use crate::deployment::{self, DeploymentSummary, UploadedSource};
use crate::domain;
use crate::error;
use crate::id::DeploymentId;
use crate::registry;
use crate::review;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, Trusted};

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
            || !admit.services.is_empty()
            || admit.version.is_some()
            || admit.upload.is_some()
        {
            return Err(error::invalid(
                "A retry ships what its Deployment froze: leave environment, services, \
                 version and upload empty",
                json!({}),
            ));
        }
        return deployment::retry(tx, who, &admit.id, source);
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
    let mut frozen = deployment::freeze(
        id,
        &saved_intent,
        &review.head.applied,
        &admit.services,
        namespace,
        cluster_domain,
    )?;
    frozen.credentials = registry::freeze(tx, id, &saved_intent, &frozen)?;
    deployment::admit(tx, who, admit, id, saved, &frozen)
}
