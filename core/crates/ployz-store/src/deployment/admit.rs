//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet; retrying queues what
//! an ended Deployment froze. Starting hands a queued one to a runner again, and
//! cancelling stops one.

use ployz_core::config::canonicalize_environment_intent;
use ployz_core::config::{SavedEnvironmentIntent, SavedVariableValue};
use ployz_core::{Namespace, RpcError, RpcErrorCode, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::deployment::{self, DeploymentStatus, DeploymentSummary, UploadedSource};
use crate::domain;
use crate::error;
use crate::id::{DeploymentId, Hostname, VolumeName};
use crate::registry;
use crate::review;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, Trusted};

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
    /// Close a Branch: once this removal applied, the Store's sweep deletes it
    /// without its admitter coming back. Ignored for an Environment that isn't a
    /// Branch; a client that deletes it itself leaves it unset.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub close: bool,
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
        // which the copied target nodes carry: nothing new, so no new review.
        Admit::Retry(retry) => {
            let retried = deployment::retry(tx, who, &retry.id, &retry.deployment)?;
            if retried.remove
                && let Some(outcome) =
                    deployment::forgettable(tx, &retried.environment_id, trusted)?
            {
                return deployment::forget(tx, &retried.id, outcome);
            }
            trusted.runnable()?;
            Ok(retried)
        }
        Admit::Remove(removal) => self::removal(tx, who, removal, trusted),
        Admit::Deploy(deploy) => self::deploy(tx, who, deploy, trusted),
    }
}

/// The gate every Deploy passes, the user's or the Store's own: a Server to run
/// it, a value for every secret, the Cluster Domain its generated domains expand
/// under, and no hostname another Namespace publishes.
pub(crate) fn gate(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, intent): (&scope::Environment, &SavedEnvironmentIntent),
    (cluster_domain, namespace): (Option<&Hostname>, &Namespace),
    trusted: &Trusted,
) -> Result<(), RpcError> {
    trusted.runnable()?;
    needs_secret_values(environment, intent)?;
    needs_cluster_domain(environment, intent, cluster_domain)?;
    domain::check_published(tx, who, intent, (cluster_domain, namespace), trusted)
}

/// Refuse to deploy a secret without a value, naming each: one that arrived by Sync
/// waits for `environment`'s own.
fn needs_secret_values(
    environment: &scope::Environment,
    intent: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let mut missing: Vec<String> = intent
        .services
        .iter()
        .flat_map(|service| {
            service
                .variables
                .iter()
                .filter(|variable| variable.value == SavedVariableValue::SecretWithoutValue)
                .map(|variable| format!("{}.env.{}", service.slug, variable.key))
        })
        .collect();
    missing.sort();
    let Some(first) = missing.first() else {
        return Ok(());
    };
    let summary = &environment.summary;
    Err(error::conflict(
        format!(
            "{} has secrets without a value: set {} before deploying",
            summary.name,
            missing.join(", ")
        ),
        json!({
            "secrets": missing,
            "next": format!("ployz set {first} --secret --project {} --env {}", summary.project, summary.name),
        }),
    ))
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
    // Cloud reserves the Cluster Domain before admitting a generated domain.
    let cluster_domain = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    let review = review::review(tx, &environment)?;
    review::check(&review, admit.version.as_deref())?;
    let id = &environment.summary.id;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let saved_intent = canonicalize_environment_intent(environment.working.clone());
    gate(
        tx,
        who,
        (&environment, &saved_intent),
        (cluster_domain, &namespace),
        trusted,
    )?;
    let losses = review::destructive(
        who,
        &review,
        (
            &saved_intent,
            &namespace,
            review::Shipping::Deploy(&admit.services),
        ),
        (admit.version.as_deref(), &admit.accept_volume_loss),
        trusted.volumes.as_ref(),
    )?;
    review::approve(who, id, &review, &saved_intent, &trusted.approval)?;
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
        (&admit.id, &admit.services, upload, admit.message.clone()),
        id,
        saved,
        &frozen,
    )
}

/// Queue the Deployment that removes an Environment from the Servers: the empty
/// Environment against everything Applied State holds, under the same destructive
/// review as any Deploy. A running Deployment must end first, so the removal's
/// targets are everything that ran. When nothing of it ever ran, or no Server is
/// left, it applies at once ([`deployment::forgettable`]): no runner, no review.
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
    let forget = deployment::forgettable(tx, id, trusted)?;
    let review = review::review(tx, &environment)?;
    review::check(&review, admit.version.as_deref())?;
    let namespace = deployment::namespace(tx, who, &environment.summary, true)?;
    let empty = crate::scope::empty(&environment.working.environment_slug);
    let losses = match forget {
        Some(_) => Vec::new(),
        None => review::destructive(
            who,
            &review,
            (&empty, &namespace, review::Shipping::Deploy(&[])),
            (admit.version.as_deref(), &admit.accept_volume_loss),
            trusted.volumes.as_ref(),
        )?,
    };
    let frozen = deployment::freeze(
        id,
        &empty,
        &review.head.applied,
        &[],
        namespace,
        None,
        &losses,
    )?;
    let admitted = deployment::admit(
        tx,
        who,
        (&admit.id, &[], None, None),
        id,
        deployment::NOTHING,
        &frozen,
    )?;
    if admit.close {
        crate::pull_request::mark_closing(tx, id)?;
    }
    if let Some(outcome) = forget {
        return deployment::forget(tx, &admitted.id, outcome);
    }
    Ok(admitted)
}
