//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Command, replayable};
use crate::Actor;
use crate::deployment::{self, DeploymentSummary};
use crate::id::DeploymentId;
use crate::registry;
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

pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Admit,
) -> Result<DeploymentSummary, RpcError> {
    let command = Command::Admit(admit.clone());
    replayable(tx, who, &command, |tx| admitted(tx, who, admit))
}

fn admitted(tx: &mut dyn Tx, who: &Actor, admit: &Admit) -> Result<DeploymentSummary, RpcError> {
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
    let saved_intent = canonicalize_environment_intent(environment.working);
    let mut frozen = deployment::freeze(
        id,
        &saved_intent,
        &review.head.applied,
        &admit.services,
        namespace,
    )?;
    frozen.credentials = registry::freeze(tx, id, &saved_intent, &frozen)?;
    deployment::admit(tx, who, &admit.id, id, saved, &admit.services, &frozen)
}
