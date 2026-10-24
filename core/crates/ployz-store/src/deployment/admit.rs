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
use crate::deployment::{self, DeploymentSummary, TargetNode, UploadedSource};
use crate::domain;
use crate::error;
use crate::id::{DeploymentId, Hostname, VolumeName};
use crate::registry;
use crate::review;
use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, Approval, Trusted};

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
    /// A new upload for the target Services without a source of their own; without
    /// one, each builds from the upload it last built from.
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
    /// Also remove every Service running in the Namespace that this Store never
    /// held: another Store's, or one deployed directly. Only with every Service;
    /// without it a Deploy removes only the Services this Store dropped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub prune: bool,
}

/// Redeploy an ended Deployment: its Saved revision, targets, Namespace,
/// credentials, upload and pinned commits, whatever was saved since. Redeploying
/// one older than a Deployment that may have changed the Servers rolls that back.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub id: DeploymentId,
    /// The Deployment it ships again.
    pub deployment: DeploymentId,
    /// The newer Deployments the caller confirmed this rolls back, by number. It
    /// refuses with `confirmation_required`, naming the fresh `rolls_back`, when
    /// they changed. None checks nothing.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub rolls_back: Option<Vec<u64>>,
    /// As [`Deploy::version`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub version: Option<String>,
    /// As [`Deploy::accept_volume_loss`]: a rollback reviews what it deletes
    /// against what the Servers hold now, never its source's consent.
    #[serde(default)]
    #[ts(as = "Option<Vec<VolumeName>>", optional)]
    pub accept_volume_loss: Vec<VolumeName>,
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
        // A plain retry deletes exactly the Docker Volumes its source's review
        // accepted, which the copied target nodes carry; a rollback reviews its
        // deletes afresh. An agent's retry asks a human, and so does a removal's.
        Admit::Retry(retry) => {
            let asked = retry_review(tx, who, &retry.deployment, trusted)?;
            let (retried, rollback) = deployment::retry(tx, who, retry, trusted)?;
            deployment::admitted_by_user(tx, &retried.id, trusted)?;
            let forget = match retried.remove {
                true => deployment::forgettable(tx, &retried.environment_id, trusted)?,
                false => None,
            };
            match asked {
                Asked::Removal(environment, review, asks) => {
                    trusted.writes(&environment.summary.name)?;
                    if forget.is_none() || asks {
                        crate::removal::ask_taking_off(
                            tx,
                            who,
                            (&environment, &review),
                            review::TakingOff::Retry(&retry.deployment),
                            trusted,
                        )?;
                    }
                }
                Asked::Retry(review, gate) => {
                    trusted
                        .writes(&scope::load_by_id(tx, &retried.environment_id)?.summary.name)?;
                    review::approve_retry(who, &review, (retry, &rollback), &gate)?;
                }
                Asked::Nothing => {}
            }
            if let Some(outcome) = forget {
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

/// What a retry asks a human about, read before it queues.
enum Asked<'a> {
    /// A removal's retry takes its Environment off the Servers again, so it asks as
    /// a shutdown does, about what the Environment runs now. The flag is whether
    /// the caller asks on every retry, as an agent does, even one with nothing left
    /// to take off.
    Removal(Box<scope::Environment>, review::Review, bool),
    /// Any other retry that may ask: the review it approves.
    Retry(review::Review, review::Gate<'a>),
    Nothing,
}

/// The review a retry of `source` asks a human to approve, taken before the retry
/// queues, so one approval admits one retry.
fn retry_review<'a>(
    tx: &mut dyn Tx,
    who: &Actor,
    source: &DeploymentId,
    trusted: &'a Trusted,
) -> Result<Asked<'a>, RpcError> {
    let summary = deployment::owned(tx, who, source)?.summary;
    let id = summary.environment_id;
    // Protection is read under the lock a Protect takes, so one that commits first
    // is the one this retry asks under.
    scope::lock_all(tx, [id.clone()])?;
    let gate = review::Gate::new(tx, &id, trusted)?;
    let gate = if summary.remove {
        gate
    } else {
        gate.retrying()
    };
    // Where Cloud requires approval, a retry that takes something off asks too.
    if !summary.remove && !gate.asks_retry() && trusted.approval == Approval::NotRequired {
        return Ok(Asked::Nothing);
    }
    let environment = scope::load_by_id(tx, &id)?;
    let review = review::review(tx, &environment)?;
    Ok(match summary.remove {
        true => Asked::Removal(Box::new(environment), review, gate.asks_retry()),
        false => Asked::Retry(review, gate),
    })
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
/// they expand under: Cloud reserves it at a Deploy. Without Cloud, removing them is
/// the way on, so the refusal names each and the commands that remove them. A Cloud
/// Deploy that comes before any Cluster Domain is reserved sees only the message.
fn needs_cluster_domain(
    environment: &scope::Environment,
    intent: &SavedEnvironmentIntent,
    cluster_domain: Option<&Hostname>,
) -> Result<(), RpcError> {
    let prefixes: Vec<&str> = domain::generated_prefixes(intent).collect();
    if cluster_domain.is_some() || prefixes.is_empty() {
        return Ok(());
    }
    let summary = &environment.summary;
    let remove = prefixes
        .iter()
        .map(|prefix| {
            format!(
                "ployz domain rm {prefix} --project {} --env {}",
                summary.project, summary.name
            )
        })
        .collect::<Vec<_>>()
        .join(" && ");
    Err(RpcError {
        code: RpcErrorCode::Unsupported,
        message: format!(
            "{} has generated domains ({}), which need the Cluster Domain Ployz Cloud \
             reserves at a Deploy: deploy through Ployz Cloud, or remove them",
            summary.name,
            prefixes.join(", ")
        ),
        details: json!({ "domains": prefixes, "next": remove }),
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
    trusted.writes(&environment.summary.name)?;
    crate::removal::marked(tx, &environment)?;
    // Cloud reserves the Cluster Domain before admitting a generated domain.
    let cluster_domain = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    if admit.prune && !admit.services.is_empty() {
        return Err(error::invalid(
            "A Deploy of named Services can't prune: deploy every Service to prune",
            json!({ "services": admit.services }),
        ));
    }
    let review = review::review(tx, &environment)?;
    let shipping = review::Shipping::Deploy {
        services: &admit.services,
        upload: admit.upload.as_ref().map(|upload| &upload.digest),
    };
    if let Err(stale) = review::check(&review, admit.version.as_deref()) {
        let target = canonicalize_environment_intent(environment.working.clone());
        let probe =
            deployment::namespace(tx, who, &environment.summary, false).and_then(|namespace| {
                review::destructive(
                    who,
                    &review,
                    (&target, &namespace, shipping),
                    (None, &[], review::consent(review.saved.as_ref())),
                    trusted.volumes.as_ref(),
                )
            });
        return Err(review::with_loss(stale, probe));
    }
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
    review::guard_saving(tx, id, review.saved.as_ref(), &saved_intent)?;
    let gate = review::Gate::new(tx, id, trusted)?;
    review::approve(who, &review, (&saved_intent, shipping), &gate)?;
    let (saved, _) = review::publish(
        tx,
        who,
        id,
        environment.working.clone(),
        review.saved.as_ref(),
        (admit.message.as_deref(), &losses),
    )?;
    let landing = deployment::Landing::of(&review.head);
    let retire = match admit.services.is_empty() {
        true => deployment::dropped(tx, id, &saved_intent, &landing)?,
        false => Vec::new(),
    };
    let scope = match admit.services.is_empty() {
        true => deployment::Scope::All {
            losses: &losses,
            retire: &retire,
            prune: admit.prune,
        },
        false => deployment::Scope::Narrow {
            services: &admit.services,
            retire: &[],
            losses: &[],
            every_saved: false,
        },
    };
    let mut frozen = deployment::freeze(
        id,
        &who.organization,
        &saved_intent,
        &landing,
        scope,
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
        (
            deployment::Admission::New {
                id: &admit.id,
                push: None,
            },
            &admit.services,
            upload,
            admit.message.clone(),
        ),
        id,
        saved,
        &frozen,
    )?;
    deployment::admitted_by_user(tx, &summary.id, trusted)?;
    // What merges landed and this Deploy ships no longer waits for a push.
    let shipped = match admit.services.is_empty() {
        true => Shipped::All(&saved_intent),
        false => Shipped::Services(
            frozen
                .nodes
                .iter()
                .filter(|node| matches!(node, TargetNode::Service { .. }))
                .map(TargetNode::id)
                .collect(),
        ),
    };
    crate::automation::shipped_landed(tx, id, &shipped)?;
    Ok(summary)
}
