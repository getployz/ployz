//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet. Cancelling stops one.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{RpcError, RpcErrorCode, ServiceName};
use serde_json::json;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Command, replayable};
use crate::domain;
use crate::{Actor, Trusted};
use crate::deployment::{self, DeploymentSummary};
use crate::id::DeploymentId;
use crate::review;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;

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
    let frozen = deployment::freeze(
        id,
        &canonicalize_environment_intent(environment.working),
        &review.head.applied,
        &admit.services,
        namespace,
        cluster_domain,
    )?;
    deployment::admit(tx, who, &admit.id, id, saved, &admit.services, &frozen)
}
