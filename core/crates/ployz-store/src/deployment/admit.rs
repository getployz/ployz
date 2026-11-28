//! Admission: freeze what a Deployment ships and queue it. Deploying publishes
//! Working State first when Saved State doesn't hold it yet; retrying queues what
//! an ended Deployment froze. Starting hands a queued one to a runner again, and
//! cancelling stops one.

use ployz_core::authority::{CopyStep, Signed};
use ployz_core::config::canonicalize_environment_intent;
use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent};
use ployz_core::{Namespace, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::deployment::{self, DeploymentSummary, PostedVersion, TargetNode, UploadedSource};
use crate::domain;
use crate::error;
use crate::followup::{FollowUp, Place};
use crate::id::{DeploymentId, EnvironmentId, Hostname, Revision, VolumeName};
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
    /// Ship some Services again as they run, under the Cluster Domain now.
    Reapply(Reapply),
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
            Self::Reapply(reapply) => &reapply.id,
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
    /// What an older client sends to prune. A Deploy of every Service prunes
    /// without it, so it changes nothing; refused with named Services or with
    /// `no_prune`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub prune: bool,
    /// Keep every Service running in the Namespace that this Store never held:
    /// another Store's, or one deployed directly. Without it, a Deploy of every
    /// Service removes them; one of named Services never does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub no_prune: bool,
    /// A signed version of each Service a client signed for this Deploy, with the image
    /// it pinned the Service's tag to. The Store keeps each for later runs and drains, and
    /// this Deployment runs those pins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<PostedVersion>>")]
    pub versions: Vec<PostedVersion>,
    /// The Copy step a person signed for each Volume this Deploy restores into a new one.
    /// The Store records the birth each mints, so every spec that mounts the new Volume
    /// carries it, even when this Deploy posts no version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<Signed<CopyStep>>>")]
    pub copies: Vec<Signed<CopyStep>>,
    /// Services whose running Containers it replaces even when their spec is unchanged,
    /// so each Server admits them again under the versions this Deploy posts. A key
    /// rotation names the Services it carries, so no Container stays kept under the key
    /// it retires, and a Service they only reference stays as it runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<ServiceName>>")]
    pub recreate: Vec<ServiceName>,
    /// The automatic deploy rule signed with this Deploy for what it saves, kept in the
    /// Deploy's own transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, as = "Option<crate::auto_rule::SavedRule>")]
    pub rule: Option<Box<crate::auto_rule::SavedRule>>,
}

/// Redeploy an ended Deployment: its Saved revision, targets, Namespace,
/// registry logins, upload, pinned commits and whether it prunes, whatever was saved
/// since. Redeploying one older than a Deployment that may have changed the
/// Servers rolls that back.
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
    /// As [`Deploy::versions`], over what its source ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<PostedVersion>>")]
    pub versions: Vec<PostedVersion>,
    /// As [`Deploy::copies`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<Signed<CopyStep>>>")]
    pub copies: Vec<Signed<CopyStep>>,
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

/// Ship Services again as Applied State holds them, with their generated domains
/// under the Cluster Domain now: how a Door switch moves them. It saves nothing,
/// so what Working and Saved State stage stays staged. Only an Actor without a
/// principal admits one: the Store's own automation or the hidden local Store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Reapply {
    pub id: DeploymentId,
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Deployed Services, each by its name in Working State, or in Applied State
    /// once Working State no longer has it.
    pub services: Vec<ServiceName>,
    /// As [`Deploy::message`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub message: Option<String>,
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
            let pruning = retry_pruning(tx, &retried, trusted)?;
            pruning.admit(trusted, Some(rollback.number))?;
            deployment::admitted_by_user(tx, &retried.id, trusted)?;
            deployment::versions::post(tx, &retried.id, (&retry.versions, &retry.copies))?;
            let forget = match retried.remove {
                true => deployment::forgettable(tx, &retried.environment_id, trusted)?,
                false => None,
            };
            match asked {
                Asked::Removal(environment, review, asks) => {
                    trusted.writes(&environment.summary.name)?;
                    if forget.is_none() || asks || trusted.approval.carried() {
                        crate::removal::ask_taking_off(
                            tx,
                            who,
                            (&environment, &review),
                            (
                                review::TakingOff::Retry(&retry.deployment),
                                forget == Some(crate::Outcome::Forgotten),
                            ),
                            trusted,
                        )?;
                    }
                }
                Asked::Retry(review, gate) => {
                    trusted
                        .writes(&scope::load_by_id(tx, &retried.environment_id)?.summary.name)?;
                    review::approve_retry(who, &review, (retry, &rollback, &pruning), &gate)?;
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
        Admit::Reapply(reapply) => self::reapply(tx, who, reapply, trusted),
    }
}

/// What a redeploy of a pruning Deployment prunes again: each Service running in
/// its Namespace that neither it nor what this Store holds now runs as.
fn retry_pruning(
    tx: &mut dyn Tx,
    retried: &DeploymentSummary,
    trusted: &Trusted,
) -> Result<review::Pruning, RpcError> {
    if !retried.prune || retried.remove {
        return Ok(review::Pruning::Off);
    }
    let stored = deployment::load(tx, &retried.id)?;
    let environment = scope::load_by_id(tx, &retried.environment_id)?;
    let head = deployment::head(tx, &environment)?;
    let landing = deployment::Landing::of(&head);
    Ok(review::Pruning::of(
        trusted.running.as_ref(),
        stored.namespace(),
        stored.nodes.iter().flat_map(TargetNode::runtimes).chain(
            landing
                .services()
                .map(|service| &service.config.private_dns),
        ),
    ))
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
    let remove = FollowUp::DomainRemove {
        prefixes: prefixes.iter().map(|prefix| (*prefix).to_owned()).collect(),
        place: Place::of(summary),
    };
    Err(RpcError::unsupported(format!(
        "{} has generated domains ({}), which need the Cluster Domain Ployz Cloud \
             reserves at a Deploy: deploy through Ployz Cloud, or remove them",
        summary.name,
        prefixes.join(", ")
    ))
    .with_details(remove.within(json!({ "domains": prefixes }))))
}

fn deploy(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Deploy,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    trusted.runnable()?;
    if admit.rule.is_some() && admit.version.is_none() {
        return Err(error::invalid(
            "A Deploy that keeps an automatic deploy rule names the review it read.",
            json!({}),
        ));
    }
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
    if admit.prune && admit.no_prune {
        return Err(error::invalid(
            "A Deploy can't both prune and keep other Services: drop one",
            json!({}),
        ));
    }
    let prunes = admit.services.is_empty() && !admit.no_prune;
    let shipping = review::Shipping::Deploy {
        services: &admit.services,
        upload: admit.upload.as_ref().map(|upload| &upload.digest),
    };
    let (review, _) = review::publish::review_for_save(
        tx,
        who,
        &environment,
        (shipping, admit.version.as_deref()),
        trusted,
    )?;
    let id = &environment.summary.id;
    let namespace = deployment::reserve_namespace(tx, who, &environment.summary)?;
    let saved_intent = canonicalize_environment_intent(environment.working.clone());
    gate(
        tx,
        who,
        (&environment, &saved_intent),
        (cluster_domain, &namespace),
        trusted,
    )?;
    let landing = deployment::Landing::of(&review.head);
    let opening = deployment::Opening::of(tx, id, &namespace)?;
    // A narrowed one freezes now; a full one freezes once saved, with what it retires.
    let narrowed = match admit.services.is_empty() {
        true => None,
        false => Some(deployment::freeze(
            id,
            &who.organization,
            &saved_intent,
            &landing,
            deployment::Scope::of(&admit.services, &[]),
            namespace.clone(),
            cluster_domain,
            opening.as_ref(),
        )?),
    };
    let pruning = match prunes {
        // What this Store holds, deploying or dropping, isn't the prune's.
        true => review::Pruning::of(
            trusted.running.as_ref(),
            &namespace,
            saved_intent
                .services
                .iter()
                .chain(deployment::Landing::of(&review.head).services())
                .map(|service| &service.config.private_dns),
        ),
        false => review::Pruning::Off,
    };
    let (saved, _, losses) = review::publish::save(
        tx,
        who,
        &review::publish::Save {
            environment: &environment,
            review: &review,
            target: &saved_intent,
            namespace: &namespace,
            shipping,
            pruning: &pruning,
            version: admit.version.as_deref(),
            accept_volume_loss: &admit.accept_volume_loss,
            message: admit.message.as_deref(),
            unchanged: review::publish::UnchangedSave::KeepsConsent,
        },
        trusted,
    )?;
    if let Some(rule) = &admit.rule {
        crate::auto_rule::keep_saved(tx, who, &environment, rule)?;
    }
    let frozen = match narrowed {
        Some(frozen) => frozen,
        None => {
            let retire = deployment::dropped(tx, id, &saved_intent, &landing)?;
            deployment::freeze(
                id,
                &who.organization,
                &saved_intent,
                &landing,
                deployment::Scope {
                    targeting: deployment::Targeting::Full { prune: prunes },
                    retire: &retire,
                    losses: &losses,
                },
                namespace,
                cluster_domain,
                opening.as_ref(),
            )?
        }
    };
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
        &environment,
        saved,
        &frozen,
    )?;
    deployment::admitted_by_user(tx, &summary.id, trusted)?;
    deployment::versions::post(tx, &summary.id, (&admit.versions, &admit.copies))?;
    deployment::versions::recreate(tx, &summary.id, &admit.recreate)?;
    // What merges landed and this Deploy ships no longer waits for a push.
    let shipped = frozen.targeting.as_ref().map(|_| {
        frozen
            .nodes
            .iter()
            .filter(|node| node.service().is_some())
            .map(TargetNode::id)
            .collect()
    });
    crate::automation::shipped_landed(tx, id, &saved_intent, &shipped)?;
    Ok(summary)
}

/// Queue a Deployment of `admit`'s Services as Applied State holds them, with their
/// generated domains under the Cluster Domain now. Refused while a Deployment is in
/// flight, since that may still change Applied State, and when no Saved revision
/// ships them all unchanged.
fn reapply(
    tx: &mut dyn Tx,
    who: &Actor,
    admit: &Reapply,
    trusted: &Trusted,
) -> Result<DeploymentSummary, RpcError> {
    if who.principal.is_some() {
        return Err(error::forbidden(
            "Only Ployz Cloud redeploys services unchanged. Deploy them instead.",
            json!({}),
        ));
    }
    trusted.runnable()?;
    let environment = scope::lock(tx, who, &admit.environment)?;
    trusted.writes(&environment.summary.name)?;
    crate::removal::marked(tx, &environment)?;
    let id = &environment.summary.id;
    if let Some(flying) = deployment::in_flight(tx, id)? {
        return Err(error::conflict(
            format!(
                "#{} is still deploying. Wait for it to finish, then try again.",
                flying.numbered()?
            ),
            json!({ "deployment": flying.id }),
        ));
    }
    let head = deployment::head(tx, &environment)?;
    let landing = deployment::Landing::of(&head);
    let goes_by = |applied: &SavedServiceIntent, name: &ServiceName| {
        environment
            .working
            .services
            .iter()
            .find(|service| service.id == applied.id)
            .map_or(applied.slug.as_str(), |service| service.slug.as_str())
            == name.as_str()
    };
    let mut running: Vec<&SavedServiceIntent> = Vec::new();
    for name in &admit.services {
        let applied = head
            .applied
            .services
            .iter()
            .find(|applied| goes_by(applied, name))
            .ok_or_else(|| {
                error::not_found(
                    format!("{name} isn't deployed."),
                    json!({ "service": name }),
                )
            })?;
        if !running.contains(&applied) {
            running.push(applied);
        }
    }
    if running.is_empty() {
        return Err(error::invalid("Name a service to redeploy.", json!({})));
    }
    let services = running
        .iter()
        .map(|service| crate::scope::service_name(service))
        .collect::<Result<Vec<_>, _>>()?;
    let namespace = deployment::reserve_namespace(tx, who, &environment.summary)?;
    let cluster_domain = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    let Some((revision, saved, frozen)) = unchanged(
        tx,
        who,
        (id, &running),
        (&services, &landing),
        (&namespace, cluster_domain),
    )?
    else {
        return Err(error::conflict(
            format!(
                "{} can't redeploy as {} now. Deploy {} or discard its changes, then try again.",
                services
                    .iter()
                    .map(ServiceName::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                if services.len() == 1 {
                    "it runs"
                } else {
                    "they run"
                },
                environment.summary.name
            ),
            json!({ "services": services }),
        ));
    };
    // The gate checks the Services the runner gets, named or referenced: an older
    // revision may give one it doesn't get a hostname released since.
    let shipping = SavedEnvironmentIntent {
        services: saved
            .services
            .iter()
            .filter(|service| frozen.hands(service))
            .cloned()
            .collect(),
        ..saved.clone()
    };
    gate(
        tx,
        who,
        (&environment, &shipping),
        (cluster_domain, &namespace),
        trusted,
    )?;
    let summary = deployment::admit(
        tx,
        who,
        (
            deployment::Admission::Beside(&admit.id),
            &services,
            None,
            admit.message.clone(),
        ),
        &environment,
        revision,
        &frozen,
    )?;
    for service in &running {
        crate::build::pin_running(tx, (id, &summary.id), service)?;
    }
    Ok(summary)
}

/// The newest Saved revision that ships each of `running`, and each Service it
/// references, as Applied State has it, frozen for `services`. It looks back to
/// the oldest revision a Deployment that put a node of Applied State in place
/// shipped, so a Saved edit since never hides one.
fn unchanged(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, running): (&EnvironmentId, &[&SavedServiceIntent]),
    (services, landing): (&[ServiceName], &deployment::Landing<'_>),
    (namespace, cluster_domain): (&Namespace, Option<&Hostname>),
) -> Result<Option<(Revision, SavedEnvironmentIntent, deployment::Frozen)>, RpcError> {
    let opening = deployment::Opening::of(tx, environment, namespace)?;
    for revision in revisions(tx, environment)? {
        let saved = deployment::saved_at(tx, environment, revision)?;
        if !running
            .iter()
            .all(|applied| saved.services.contains(applied))
        {
            continue;
        }
        let frozen = deployment::freeze(
            environment,
            &who.organization,
            &saved,
            landing,
            deployment::Scope::of(services, &[]),
            namespace.clone(),
            cluster_domain,
            opening.as_ref(),
        )?;
        let ships_as_it_runs = frozen.nodes.iter().all(|node| {
            saved
                .services
                .iter()
                .filter(|service| service.id == node.id())
                .all(|service| !deployment::changed(&saved, landing, service))
        });
        if ships_as_it_runs {
            return Ok(Some((revision, saved, frozen)));
        }
    }
    Ok(None)
}

/// The Saved revisions of `environment` a reapply may ship, newest first: each one
/// since the oldest that a Deployment putting a node of Applied State in place
/// shipped.
fn revisions(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Vec<Revision>, RpcError> {
    tx.query(
        "SELECT s.revision FROM config_saved s WHERE s.environment_id = ?1 \
         AND s.revision >= (SELECT MIN(d.saved_revision) FROM config_applied a \
         JOIN config_deployment d ON d.id = a.deployment_id WHERE a.environment_id = ?1) \
         ORDER BY s.revision DESC",
        &[environment.as_str().into()],
    )?
    .iter()
    .map(|row| row.number(0, "revision"))
    .collect()
}
