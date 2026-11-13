//! Deployment reads: the authored plan of a Deploy, one page of an Environment's
//! Deployments, and one Deployment with its Node Outcomes.

use ployz_core::config::{SavedEnvironmentIntent, canonicalize_environment_intent};
use ployz_core::{RequestedServiceSpec, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::deployment::loss::{VolumeLoss, removed};
use crate::deployment::{self, DeploymentSummary, DeploymentView};
use crate::error;
use crate::followup::{FollowUp, Place};
use crate::id::{DeploymentId, Hostname};
use crate::review::{self, NodeChange};
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;

/// What `deploy` would ship, from authored state alone: nothing builds or runs.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PlanQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Plan only these Services; none plans every Service.
    #[serde(default)]
    pub services: Vec<ServiceName>,
    /// Also lower the plan as a runner would, for a Device to approve the spec each
    /// Service runs. Needs write access to the Environment.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub target: bool,
    /// Plan a redeploy of this ended Deployment of the Environment instead: what it
    /// froze, lowered as its redeploy's runner will lower it, for a Device to approve.
    /// It names no Services and lists no changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub deployment: Option<DeploymentId>,
}

/// The authored review of a Deploy. What only the Servers can decide is `unresolved`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct PlanView {
    pub environment: EnvironmentSummary,
    /// Pass to `deploy --expect-version` to ship exactly this.
    pub version: String,
    pub namespace: ployz_core::Namespace,
    /// The changes of the nodes this Deploy targets.
    pub changes: Vec<NodeChange>,
    /// What the Deployment decides from the Servers: which containers start, stop or
    /// move, and where.
    pub unresolved: Vec<String>,
    /// The spec of each Service this Deploy ships, as its runner would ask a Server to
    /// run it, when the query asked for `target`. A narrowed Deploy lists the Services
    /// it names and those they reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target: Option<Vec<RequestedServiceSpec>>,
}

/// Where an Environment runs on the Servers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NamespaceQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
}

/// The Namespace an Environment's containers carry: fixed at its first Deployment,
/// else the one its first Deployment would take.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NamespaceView {
    pub environment: EnvironmentSummary,
    pub namespace: ployz_core::Namespace,
    /// Each deployed Service's runtime name (its Private DNS name, as Applied State
    /// has it), by the name it has now: a renamed Service's containers keep the name
    /// it was created with, and a staged Private DNS change isn't live yet.
    pub services: std::collections::BTreeMap<ServiceName, ServiceName>,
    /// Every Service in Working State, deployed or not. Absent from an older Store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authored: Option<std::collections::BTreeSet<ServiceName>>,
    /// Every hostname the Namespace serves as Applied State has it, by what produced
    /// it. A name the Servers publish that isn't here, no Deployment Cloud knows of put
    /// there. Absent from an older Store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub routes: Option<Vec<AppliedRoute>>,
}

/// A hostname Applied State serves in the Namespace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct AppliedRoute {
    pub hostname: Hostname,
    /// The runtime name of the Service serving it, as its containers carry it.
    pub service: ServiceName,
    pub kind: RouteKind,
}

/// What produced a route: a generated domain, expanded under the Cluster Domain its
/// Deployment used, or a custom domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    Generated,
    Custom,
}

/// Every Namespace the Organization's Environments own on the Servers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NamespacesQuery {}

/// The Organization's Namespaces, each with the Environment that owns it. A
/// Namespace the Servers run that isn't here is in no Project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NamespacesView {
    pub namespaces: Vec<OwnedNamespace>,
}

/// A Namespace and the Environment that owns it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OwnedNamespace {
    pub namespace: ployz_core::Namespace,
    pub project: crate::ProjectName,
    pub environment: crate::EnvironmentName,
}

/// One page of an Environment's Deployments, newest first.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploymentsQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// At most this many, 1-100 [default: 20].
    #[serde(default)]
    pub limit: Option<usize>,
    /// The `next_cursor` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// One page of Deployments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DeploymentsView {
    pub environment: EnvironmentSummary,
    pub deployments: Vec<DeploymentSummary>,
    /// Pass as `cursor` for the next page; none on the last.
    pub next_cursor: Option<String>,
    /// The Environment's newest numbered Deployment, whatever page it falls on:
    /// holds waiting for CI take no number, so a burst of them never hides it.
    /// None before the first Deploy.
    pub latest: Option<DeploymentSummary>,
    /// Its Deployments in flight (queued, running, cancelling), newest number
    /// first, whatever page they fall on: holds made since push them down the
    /// list, however few rows a page holds.
    pub in_flight: Vec<DeploymentSummary>,
}

/// One Deployment, by ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploymentQuery {
    pub id: DeploymentId,
}

/// One Deployment, by its number in an Environment; or its latest numbered one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NumberedDeploymentQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// None: the latest Deployment that was numbered. A hold waiting for CI has
    /// no number and is never the latest.
    #[serde(default)]
    pub number: Option<u64>,
}

pub(crate) fn numbered(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &NumberedDeploymentQuery,
) -> Result<DeploymentView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let id = &environment.summary.id;
    let rows = match query.number {
        Some(number) => tx.query(
            "SELECT id FROM config_deployment WHERE environment_id = ?1 AND number = ?2",
            &[
                id.as_str().into(),
                i64::try_from(number).unwrap_or(i64::MAX).into(),
            ],
        )?,
        None => tx.query(
            "SELECT id FROM config_deployment \
             WHERE environment_id = ?1 AND number IS NOT NULL ORDER BY number DESC LIMIT 1",
            &[id.as_str().into()],
        )?,
    };
    let Some(row) = rows.first() else {
        let (project, name) = (&environment.summary.project, &environment.summary.name);
        return Err(match query.number {
            Some(number) => error::not_found(
                format!("{project}/{name} has no Deployment #{number}"),
                FollowUp::DeploymentList {
                    place: Place::of(&environment.summary),
                }
                .details(),
            ),
            None => error::not_found(
                format!("{project}/{name} has no Deployment yet"),
                FollowUp::Deploy { no_prune: false }.details(),
            ),
        });
    };
    let id = row.parse(0, "identity")?;
    deployment::view(tx, who, &id)
}

pub(crate) fn plan(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &crate::Trusted,
    query: &PlanQuery,
) -> Result<PlanView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    if let Some(id) = &query.deployment {
        return redeploy_plan(tx, who, trusted, (environment, id), query);
    }
    let mut review = review::review(tx, &environment)?;
    let rows = review::service_rows(&mut review.view.changes);
    crate::domain::name_generated(tx, &environment, &review.head, trusted, rows)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    let target = canonicalize_environment_intent(environment.working.clone());
    // Which Docker Volumes a removal deletes only the Servers can say. Naming
    // Services saves Working State too, which reviews what it drops.
    let saves = review
        .saved
        .as_ref()
        .is_none_or(|saved| saved.intent != target);
    let removed: Vec<VolumeLoss> = if query.services.is_empty() || saves {
        removed(
            &review::approval::deployed(&review.head),
            &target,
            &namespace,
        )?
    } else {
        Vec::new()
    }
    .into_iter()
    .map(|volume| VolumeLoss {
        volume,
        deletes: Vec::new(),
    })
    .collect();
    let mut unresolved = vec!["operations".to_owned()];
    if !removed.is_empty() {
        unresolved.push("volume data".to_owned());
    }
    let frozen = deployment::freeze(
        &environment.summary.id,
        &who.organization,
        &target,
        &deployment::Landing::of(&review.head),
        deployment::Scope::of(&query.services, &removed),
        namespace.clone(),
        None,
    )?;
    let lowered = match query.target {
        true => Some(lowered_target(
            tx,
            who,
            trusted,
            (&environment, &target),
            query,
        )?),
        false => None,
    };
    let changes = review
        .view
        .changes
        .into_iter()
        .filter_map(|mut change| {
            change.restarts.retain(|service| frozen.ships(service));
            (frozen.targets(&change.node.id) || !change.restarts.is_empty()).then_some(change)
        })
        .collect();
    Ok(PlanView {
        environment: environment.summary,
        version: review.view.version,
        namespace,
        changes,
        unresolved,
        target: lowered,
    })
}

/// A redeploy of Deployment `id` of `environment`, which ships its source's frozen
/// revision, Namespace and Cluster Domain whatever was saved since. With `target`, the
/// specs its runner will lower at claim, by the same lowering, for a Device to approve
/// before it admits the redeploy.
fn redeploy_plan(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &crate::Trusted,
    (environment, id): (scope::Environment, &DeploymentId),
    query: &PlanQuery,
) -> Result<PlanView, RpcError> {
    if !query.services.is_empty() {
        return Err(error::invalid(
            "A redeploy ships what its Deployment shipped: name no Services",
            json!({ "deployment": id, "services": query.services }),
        ));
    }
    let stored = deployment::owned(tx, who, id)?;
    if stored.summary.environment_id != environment.summary.id {
        return Err(error::not_found(
            format!("{} has no such Deployment", environment.summary.name),
            json!({ "deployment": id }),
        ));
    }
    let target = match query.target {
        true => {
            trusted.writes(&environment.summary.name)?;
            let saved = deployment::held_revision(tx, &stored)?;
            let lowered = deployment::lowered(tx, &stored, &saved)?;
            let mut target = lowered.intent.target;
            // A narrowed Deployment creates only what it ships, as a narrowed Deploy does.
            if !stored.summary.services.is_empty() {
                target.retain(|spec| lowered.shipped.contains(&spec.name));
            }
            Some(target)
        }
        false => None,
    };
    let version = review::review(tx, &environment)?.view.version;
    Ok(PlanView {
        namespace: stored.namespace().clone(),
        environment: environment.summary,
        version,
        changes: Vec::new(),
        unresolved: vec!["operations".to_owned()],
        target,
    })
}

/// `target` lowered as a claim of a Deploy of it would: the specs of the Services it
/// ships, for a Device to approve before it admits the Deploy.
fn lowered_target(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &crate::Trusted,
    (environment, target): (&scope::Environment, &SavedEnvironmentIntent),
    query: &PlanQuery,
) -> Result<Vec<RequestedServiceSpec>, RpcError> {
    trusted.writes(&environment.summary.name)?;
    saved_target(tx, who, trusted, (environment, target), &query.services)
}

/// `target` lowered as a claim of a Deploy of it would, narrowed to `services` unless
/// that is empty. An automatic deploy rule approves these specs, less their images.
pub(crate) fn saved_target(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &crate::Trusted,
    (environment, target): (&scope::Environment, &SavedEnvironmentIntent),
    services: &[ServiceName],
) -> Result<Vec<RequestedServiceSpec>, RpcError> {
    saved_target_setup(tx, who, trusted, (environment, target), services, true)
}

/// Every Service of `target` lowered as [`saved_target`] lowers it, with a Branch's
/// Setup Commands only when `setup`: a preview rule approves both forms.
pub(crate) fn saved_target_with(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &crate::Trusted,
    (environment, target): (&scope::Environment, &SavedEnvironmentIntent),
    setup: bool,
) -> Result<Vec<RequestedServiceSpec>, RpcError> {
    saved_target_setup(tx, who, trusted, (environment, target), &[], setup)
}

fn saved_target_setup(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &crate::Trusted,
    (environment, target): (&scope::Environment, &SavedEnvironmentIntent),
    services: &[ServiceName],
    setup: bool,
) -> Result<Vec<RequestedServiceSpec>, RpcError> {
    let id = &environment.summary.id;
    let mut saved = target.clone();
    crate::volume::size::held(tx, id, &mut saved.volumes)?;
    let mut branch = crate::branch::lowering(tx, id, &saved)?;
    if !setup {
        branch.setup.clear();
    }
    let deployed = deployment::deployed_volumes(tx, id, &saved)?;
    let removals = deployment::ServiceRemovals::default();
    let ships = match services.is_empty() {
        true => deployment::Ships::All {
            removals,
            prune: false,
        },
        false => deployment::Ships::Named {
            services: services.to_vec(),
            removals,
        },
    };
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    let cluster_domain = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    deployment::lower(
        id,
        &saved,
        &deployed,
        &ships,
        (namespace, cluster_domain),
        &branch,
        &who.organization,
    )
    .map(|lowered| {
        let mut target = lowered.intent.target;
        // A narrowed Deploy creates only what it ships; the rest ride along unchanged.
        if !services.is_empty() {
            target.retain(|spec| lowered.shipped.contains(&spec.name));
        }
        target
    })
}

/// Every Namespace `who`'s Environments own.
pub(crate) fn namespaces(tx: &mut dyn Tx, who: &Actor) -> Result<NamespacesView, RpcError> {
    let rows = tx.query(
        "SELECT n.namespace, p.name, e.name FROM config_namespace n \
         JOIN config_environment e ON e.id = n.environment_id \
         JOIN config_project p ON p.id = e.project_id \
         WHERE n.organization_id = ?1 ORDER BY n.namespace",
        &[who.organization.as_str().into()],
    )?;
    let namespaces = rows
        .iter()
        .map(|row| {
            Ok(OwnedNamespace {
                namespace: row.parse(0, "Namespace")?,
                project: row.parse(1, "Project name")?,
                environment: row.parse(2, "Environment name")?,
            })
        })
        .collect::<Result<_, RpcError>>()?;
    Ok(NamespacesView { namespaces })
}

pub(crate) fn namespace(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &NamespaceQuery,
) -> Result<NamespaceView, RpcError> {
    let environment = scope::environment(tx, who, &query.environment)?;
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    let head = deployment::head(tx, &environment)?;
    let clusters = deployment::cluster_domains(tx, &environment.summary.id, &head)?;
    let applied = &head.applied;
    // Containers run as Applied State has them, under the name the Service has now.
    let services = applied
        .services
        .iter()
        .map(|applied| {
            let now = environment
                .working
                .services
                .iter()
                .find(|service| service.id == applied.id)
                .unwrap_or(applied);
            (now.slug.clone(), applied.config.private_dns.clone())
        })
        .collect();
    let authored = environment
        .working
        .services
        .iter()
        .map(|service| service.slug.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut routes = Vec::new();
    for service in &applied.services {
        let mut route = |hostname: &str, kind| {
            if let Ok(hostname) = Hostname::parse(hostname) {
                routes.push(AppliedRoute {
                    hostname,
                    service: service.config.private_dns.clone(),
                    kind,
                });
            }
        };
        for held in &service.config.routes {
            route(&held.hostname, RouteKind::Custom);
        }
        for managed in &service.config.managed_hostnames {
            if let Some(cluster) = clusters.get(&(service.id.clone(), managed.prefix.clone())) {
                route(
                    &format!("{}.{cluster}", managed.prefix),
                    RouteKind::Generated,
                );
            }
        }
    }
    Ok(NamespaceView {
        environment: environment.summary,
        namespace,
        services,
        authored: Some(authored),
        routes: Some(routes),
    })
}

pub(crate) fn page(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &DeploymentsQuery,
) -> Result<DeploymentsView, RpcError> {
    let limit = query.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(error::invalid("Expected a limit from 1 to 100", json!({})));
    }
    let cursor = query
        .cursor
        .as_deref()
        .map(|cursor| {
            deployment::Cursor::decode(cursor)
                .ok_or_else(|| error::invalid("Expected a cursor from a previous page", json!({})))
        })
        .transpose()?;
    let environment = scope::environment(tx, who, &query.environment)?;
    let (deployments, next) =
        deployment::page(tx, &environment.summary.id, limit, cursor.as_ref())?;
    let latest = deployment::latest_numbered(tx, &environment.summary.id)?;
    let in_flight = deployment::all_in_flight(tx, &environment.summary.id)?;
    Ok(DeploymentsView {
        environment: environment.summary,
        deployments,
        next_cursor: next.map(|cursor| cursor.encode()),
        latest,
        in_flight,
    })
}
