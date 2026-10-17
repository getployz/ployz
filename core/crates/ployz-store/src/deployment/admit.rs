//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet; retrying queues what
//! an ended Deployment froze. Starting hands a queued one to a runner again, and
//! cancelling stops one.

use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::config::canonicalize_environment_intent;
use ployz_core::{Namespace, RpcError, RpcErrorCode, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::automation::Shipped;
use crate::deployment::{self, DeploymentSummary, UploadedSource};
use crate::domain;
use crate::id::{DeploymentId, Hostname, VolumeName};
use crate::registry;
use crate::review;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, Approval, Asks, Trusted};

/// Queue a Deployment: a Deploy of Saved State, a retry of an ended Deployment, or
/// the removal of an Environment from the Servers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "admit", rename_all = "snake_case")]
pub enum Admit {
    /// Publish Working State if needed, then deploy all of it or some Services.
    Deploy(Deploy),
    /// Ship again exactly what an ended Deployment froze.
    Retry(Retry),
    /// Take an Environment off the Servers under the removal that names it.
    Remove(AdmitRemoval),
    /// Take an Environment off the Servers and keep it.
    ShutDown(ShutDown),
}

impl Admit {
    /// The new Deployment's ID.
    #[must_use]
    pub const fn id(&self) -> &DeploymentId {
        match self {
            Self::Deploy(deploy) => &deploy.id,
            Self::Retry(retry) => &retry.id,
            Self::Remove(removal) => &removal.id,
            Self::ShutDown(shut_down) => &shut_down.id,
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
    /// What this Deploy ships, in the admitter's words; shown on the Deployment.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub message: Option<String>,
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

/// Take an Environment off the Servers under the `remove` that marked it, or its
/// Project: what that removal accepted is the consent, so nothing asks again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AdmitRemoval {
    pub id: DeploymentId,
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// Take an Environment off the Servers and keep it in the Store: ship it empty,
/// deleting its deployed Volumes, without touching Working or Saved State.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ShutDown {
    pub id: DeploymentId,
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// As [`Deploy::version`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
    /// As [`Deploy::accept_volume_loss`]. Naming any asks a human first.
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

pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Admit,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    match admit {
        // The retry deletes exactly the Docker Volumes its source's review accepted,
        // which the copied target nodes carry: nothing new, so no new destructive
        // review. Only an agent's retry asks a human.
        Admit::Retry(retry) => {
            let asked = retry_review(tx, who, &retry.deployment, trusted)?;
            let retried = deployment::retry(tx, who, &retry.id, &retry.deployment)?;
            if let Some(review) = asked {
                review::approve_retry(
                    who,
                    &retried.environment_id,
                    &review,
                    &retry.deployment,
                    trusted,
                )?;
            }
            if retried.remove
                && let Some(outcome) =
                    deployment::forgettable(tx, &retried.environment_id, trusted)?
            {
                return deployment::forget(tx, &retried.id, outcome);
            }
            trusted.runnable()?;
            Ok(retried)
        }
        Admit::Remove(removal) => crate::removal::admit(tx, who, removal, trusted),
        Admit::ShutDown(shut_down) => crate::removal::shut_down(tx, who, shut_down, trusted),
        Admit::Deploy(deploy) => self::deploy(tx, who, deploy, trusted),
    }
}

/// The review a retry of `source` asks a human to approve, taken before the retry
/// queues: none unless every Deploy asks.
fn retry_review(
    tx: &mut dyn Tx,
    who: &Actor,
    source: &DeploymentId,
    trusted: &Trusted,
) -> Result<Option<review::Review>, RpcError> {
    if trusted.asks == Asks::Destroys || trusted.approval == Approval::NotRequired {
        return Ok(None);
    }
    let id = deployment::owned(tx, who, source)?.summary.environment_id;
    scope::lock_all(tx, [id.clone()])?;
    let environment = scope::load_by_id(tx, &id)?;
    review::review(tx, &environment).map(Some)
}

/// The gate every Deploy passes, the user's or the Store's own: a Server to run
/// it, the Cluster Domain its generated domains expand under, and no hostname
/// another Namespace publishes. A secret without a value never blocks it: the
/// Deployment leaves it out and warns.
pub(crate) fn gate(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, intent): (&scope::Environment, &SavedEnvironmentIntent),
    (cluster_domain, namespace): (Option<&Hostname>, &Namespace),
    trusted: &Trusted,
) -> Result<(), RpcError> {
    trusted.runnable()?;
    needs_cluster_domain(environment, intent, cluster_domain)?;
    domain::check_published(tx, who, intent, (cluster_domain, namespace), trusted)
}

/// Refuse to deploy `environment`'s generated domains without the Cluster Domain
/// they expand under: Cloud reserves it at a Deploy.
fn needs_cluster_domain(
    environment: &scope::Environment,
    intent: &SavedEnvironmentIntent,
    cluster_domain: Option<&Hostname>,
) -> Result<(), RpcError> {
    if cluster_domain.is_some() || !domain::has_generated(intent) {
        return Ok(());
    }
    Err(RpcError {
        code: RpcErrorCode::Unsupported,
        message: format!(
            "{} has generated domains, which need the Cluster Domain Ployz Cloud reserves \
             at a Deploy: deploy it through Ployz Cloud first",
            environment.summary.name
        ),
        details: json!({}),
        cause: Vec::new(),
    })
}

fn deploy(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Deploy,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    trusted.runnable()?;
    let environment = scope::lock(tx, who, &admit.environment)?;
    crate::removal::marked(tx, &environment)?;
    // Cloud reserves the Cluster Domain before admitting a generated domain.
    let cluster_domain = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    let review = review::review(tx, &environment)?;
    review::check(&review, admit.version.as_deref())?;
    let id = &environment.summary.id;
    // A manual Deploy ends the draft's proposals: what they brought is its own now.
    crate::branch::end_included(tx, id, None)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let saved_intent = canonicalize_environment_intent(environment.working.clone());
    gate(
        tx,
        who,
        (&environment, &saved_intent),
        (cluster_domain, &namespace),
        trusted,
    )?;
    let shipping = review::Shipping::Deploy {
        services: &admit.services,
        upload: admit.upload.as_ref().map(|upload| &upload.digest),
    };
    let losses = review::destructive(
        who,
        &review,
        (&saved_intent, &namespace, shipping),
        (
            admit.version.as_deref(),
            &admit.accept_volume_loss,
            review::consent(review.saved.as_ref()),
        ),
        trusted.volumes.as_ref(),
    )?;
    review::approve(who, id, &review, (&saved_intent, shipping), trusted)?;
    let (saved, _) = review::publish(
        tx,
        who,
        id,
        environment.working.clone(),
        review.saved.as_ref(),
        (admit.message.as_deref(), &review::lost(losses.clone())),
    )?;
    let mut frozen = deployment::freeze(
        id,
        &saved_intent,
        &review.head.applied,
        deployment::Scope::of(&admit.services, &losses),
        namespace,
        cluster_domain,
    )?;
    frozen.credentials = registry::freeze(tx, id, &saved_intent, &frozen)?;
    // Only Cloud's authentication names an uploader, never the caller.
    let upload = admit.upload.clone().map(|upload| UploadedSource {
        uploader: who.principal.clone(),
        ..upload
    });
    let summary = deployment::admit(
        tx,
        who,
        (&admit.id, &admit.services, upload, admit.message.clone()),
        id,
        saved,
        &frozen,
    )?;
    // What merges landed and this Deploy ships no longer waits for a push.
    let shipped = match admit.services.is_empty() {
        true => Shipped::All(&saved_intent),
        false => Shipped::Services(
            saved_intent
                .services
                .iter()
                .filter(|service| {
                    admit
                        .services
                        .iter()
                        .any(|name| name.as_str() == service.slug)
                })
                .map(|service| service.id.as_str())
                .collect(),
        ),
    };
    crate::automation::shipped_landed(tx, id, &shipped)?;
    Ok(summary)
}
