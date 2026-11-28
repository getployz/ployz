//! Deployment reads: the authored plan of a Deploy, one page of an Environment's
//! Deployments, and one Deployment with its Node Outcomes.

use ployz_core::authority::PlannedVolumes;
use ployz_core::config::{SavedEnvironmentIntent, canonicalize_environment_intent};
use ployz_core::{Region, RequestedServiceSpec, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU8;
use ts_rs::TS;

use crate::Actor;
use crate::deployment::loss::{VolumeLoss, removed};
use crate::deployment::{self, DeploymentSummary, DeploymentView};
use crate::error;
use crate::followup::{FollowUp, Place};
use crate::id::{DeploymentId, EnvironmentId, Hostname};
use crate::review::{self, NodeChange};
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::storage::Tx;

/// What a plan leaves for the Servers to decide when the Deployment runs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(from = "String")]
pub enum Unresolved {
    /// Which containers start and stop, and where.
    #[serde(rename = "operations")]
    Operations,
    /// Which Volume data is deleted.
    #[serde(rename = "volume data")]
    VolumeData,
    /// One a newer Store names, as it wrote it.
    #[serde(untagged)]
    Other(String),
}

impl From<String> for Unresolved {
    fn from(said: String) -> Self {
        match said.as_str() {
            "operations" => Self::Operations,
            "volume data" => Self::VolumeData,
            _ => Self::Other(said),
        }
    }
}

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
    #[ts(as = "Vec<String>")]
    pub unresolved: Vec<Unresolved>,
    /// The spec of each Service this Deploy ships, as its runner would ask a Server to
    /// run it, when the query asked for `target`. A narrowed Deploy lists the Services
    /// it names and those they reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target: Option<Vec<RequestedServiceSpec>>,
    /// The region each Service this Deploy moves runs in next; none for one that leaves
    /// a region, or its counts by region, to run on any Server. A Service that stays in
    /// its region, or on any Server, is left out, as is one that sets its replicas by
    /// region.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<BTreeMap<ServiceName, Option<Region>>>")]
    pub regions: BTreeMap<ServiceName, Option<Region>>,
    /// The replica counts of each Service this Deploy ships that sets them by region.
    /// Only the Servers say which of those regions can run it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<BTreeMap<ServiceName, BTreeMap<Region, u8>>>")]
    pub replicas: BTreeMap<ServiceName, BTreeMap<Region, NonZeroU8>>,
    /// The Volumes `target` may mount, by the name it declares each under, when the query
    /// asked for `target`: which ones the approval mints a birth for.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<PlannedVolumes>")]
    pub volumes: PlannedVolumes,
    /// When the query asked for `target`: each Service this Deploy ships that reaches data
    /// and that no kept signed version covers as its claim would run it, with what it
    /// needs. A person signs the ones that need a version. Absent from an older Store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<Unsigned>>")]
    pub unsigned: Vec<Unsigned>,
    /// When the query asked for `target`: each Service this Deploy ships, by the name it
    /// runs under, that builds from a Git repository and that the live automatic deploy
    /// rule doesn't approve as it stands, whether or not it reaches data. Only a Source
    /// step signed with the Deploy builds it. Absent from an older Store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<ServiceName>>")]
    pub sources: Vec<ServiceName>,
}

/// A Service a Deploy ships that its Servers run only signed, and nothing signed covers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Unsigned {
    pub service: ServiceName,
    pub needs: Needs,
    /// For [`Needs::Rule`]: the person ended the rule, which only the Deploy every push
    /// switch turns back on. A field of its own, so a client that predates it still reads
    /// the plan.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub ended: bool,
}

/// What a Service in [`PlanView::unsigned`] needs before a person's Deploy can run it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Needs {
    /// A version the person signs.
    Version,
    /// Its automatic deploy rule approved again: it builds from source, and no live rule
    /// that tracks its branch approves its spec as it stands. The Deploy's own tap signs
    /// the rule, and the Deploy builds under it.
    Rule,
    /// A build under its rule: the rule approves its spec, but no build under the rule
    /// has deployed it yet. A push to its branch makes one, and so does a Deploy.
    Rebuild,
    /// Its image written by digest: it pulls a tag with a login, which no public lookup
    /// can pin for a person to sign.
    Digest,
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
    /// The region its Services run in as Working State has it, for the next Deploy; none
    /// when it runs in any region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(as = "Option<String>", optional)]
    pub region: Option<ployz_core::Region>,
    /// Working State could not be read, so `region` and `replica_regions` say nothing:
    /// where its Services run next is unknown, not any.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub unread: bool,
    /// The regions each Service that sets its replicas by region runs in, by name. Such
    /// a Service runs there whatever region the Environment names. Keyed by the Service's
    /// name and its Private DNS name, the one its containers carry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(
        optional,
        as = "Option<BTreeMap<ServiceName, Vec<ployz_core::Region>>>"
    )]
    pub replica_regions: BTreeMap<ServiceName, Vec<ployz_core::Region>>,
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
    let namespace = deployment::namespace(tx, who, &environment.summary)?;
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
    let mut unresolved = vec![Unresolved::Operations];
    if !removed.is_empty() {
        unresolved.push(Unresolved::VolumeData);
    }
    let landing = deployment::Landing::of(&review.head);
    let opening = deployment::Opening::of(tx, &environment.summary.id, &namespace)?;
    let frozen = deployment::freeze(
        &environment.summary.id,
        &who.organization,
        &target,
        &landing,
        deployment::Scope::of(&query.services, &removed),
        namespace.clone(),
        None,
        opening.as_ref(),
    )?;
    let (lowered, volumes, unsigned, sources) = match query.target {
        true => {
            // The specs of the Services it ships, for a Device to approve before it
            // admits the Deploy.
            trusted.writes(&environment.summary.name)?;
            let lowered = saved_target(tx, who, trusted, (&environment, &target), &query.services)?;
            let unsigned = deployment::versions::unsigned(
                tx,
                (
                    &environment.summary.id,
                    who.organization.as_str(),
                    &namespace,
                ),
                &target,
                (&frozen.nodes, &lowered),
            )?;
            let volumes = crate::volume::birth::planned(tx, &environment.summary.id, &target)?;
            let sources = deployment::versions::unsourced(
                tx,
                (
                    &environment.summary.id,
                    who.organization.as_str(),
                    &namespace,
                ),
                &target,
                (&frozen.nodes, &lowered),
            )?;
            (Some(lowered), volumes, unsigned, sources)
        }
        false => (None, PlannedVolumes::new(), Vec::new(), Vec::new()),
    };
    let regions = target
        .services
        .iter()
        .filter(|service| service.config.replicas.per_region().is_none())
        .filter(|service| landing.changes_region(service, target.region.as_ref()))
        .filter_map(|service| ServiceName::parse(service.slug.as_str()).ok())
        .filter(|service| frozen.ships(service))
        .map(|service| (service, target.region.clone()))
        .collect();
    let replicas = target
        .services
        .iter()
        .filter_map(|service| {
            let counts = service.config.replicas.per_region()?;
            let service = ServiceName::parse(service.slug.as_str()).ok()?;
            frozen.ships(&service).then(|| (service, counts.clone()))
        })
        .collect();
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
        regions,
        replicas,
        volumes,
        unsigned,
        sources,
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
    let (target, volumes) = match query.target {
        true => {
            trusted.writes(&environment.summary.name)?;
            let saved = deployment::held_revision(tx, &stored)?;
            let target =
                deployment::lowered(tx, &stored, &saved)?.created(!stored.targeting().is_full());
            let volumes = crate::volume::birth::planned(tx, &environment.summary.id, &saved)?;
            (Some(target), volumes)
        }
        false => (None, PlannedVolumes::new()),
    };
    let version = review::review(tx, &environment)?.view.version;
    Ok(PlanView {
        namespace: stored.namespace().clone(),
        environment: environment.summary,
        version,
        changes: Vec::new(),
        unresolved: vec![Unresolved::Operations],
        target,
        regions: BTreeMap::new(),
        replicas: BTreeMap::new(),
        volumes,
        unsigned: Vec::new(),
        sources: Vec::new(),
    })
}

/// `saved`, `environment`'s latest Saved revision, lowered as a full Deploy of a preview of
/// it in `preview` would be, its generated domains under the Cluster Domain of the Parent's
/// last Deploy as a preview's are: what a preview rule approves, less each built image.
pub(crate) fn preview_target(
    tx: &mut dyn Tx,
    who: &Actor,
    (environment, saved): (&EnvironmentId, &SavedEnvironmentIntent),
    preview: &ployz_core::PreviewNamespace,
) -> Result<Vec<RequestedServiceSpec>, RpcError> {
    let ships = deployment::Ships {
        targeting: deployment::Targeting::Full { prune: false },
        removals: deployment::ServiceRemovals::default(),
    };
    let cluster_domain = deployment::cluster_domain(tx, environment)?;
    let opening = deployment::Opening::sample(environment.clone(), saved.clone(), preview.clone());
    deployment::lower(
        environment,
        saved,
        (&BTreeSet::new(), &BTreeMap::new(), &BTreeMap::new()),
        &ships,
        (preview.namespace().clone(), cluster_domain.as_ref()),
        &crate::branch::Lowering::default(),
        &who.organization,
        Some(&opening),
    )
    .map(|lowered| lowered.created(false))
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
    let id = &environment.summary.id;
    let mut saved = target.clone();
    crate::volume::size::held(tx, id, &mut saved.volumes)?;
    let ships = deployment::Ships {
        targeting: match services.is_empty() {
            true => deployment::Targeting::Full { prune: false },
            false => deployment::Targeting::Named(services.to_vec()),
        },
        removals: deployment::ServiceRemovals::default(),
    };
    let namespace = deployment::namespace(tx, who, &environment.summary)?;
    let cluster_domain = trusted
        .domains
        .cluster_domain
        .as_ref()
        .map(|cluster| &cluster.name);
    deployment::lower_held(
        tx,
        id,
        &saved,
        &ships,
        (namespace, cluster_domain),
        &who.organization,
    )
    .map(|lowered| lowered.created(!services.is_empty()))
}

/// Every Namespace `who`'s Environments own.
pub(crate) fn namespaces(tx: &mut dyn Tx, who: &Actor) -> Result<NamespacesView, RpcError> {
    let rows = tx.query(
        "SELECT n.namespace, p.name, e.name, e.working FROM config_namespace n \
         JOIN config_environment e ON e.id = n.environment_id \
         JOIN config_project p ON p.id = e.project_id \
         WHERE n.organization_id = ?1 ORDER BY n.namespace",
        &[who.organization.as_str().into()],
    )?;
    let namespaces = rows
        .iter()
        .map(|row| {
            // Cloud reads this view to name the Environment whose Working State it can't read.
            let intent = row.intent(3, "Working State").ok();
            let mut replica_regions = BTreeMap::new();
            for service in intent.iter().flat_map(|intent| &intent.services) {
                let regions: Vec<_> = service.config.replicas.regions().cloned().collect();
                if regions.is_empty() {
                    continue;
                }
                for name in [service.slug.as_str(), service.config.private_dns.as_str()] {
                    if let Ok(name) = ServiceName::parse(name) {
                        replica_regions.insert(name, regions.clone());
                    }
                }
            }
            Ok(OwnedNamespace {
                namespace: row.parse(0, "Namespace")?,
                project: row.parse(1, "Project name")?,
                environment: row.parse(2, "Environment name")?,
                unread: intent.is_none(),
                region: intent.and_then(|intent| intent.region),
                replica_regions,
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
    let namespace = deployment::namespace(tx, who, &environment.summary)?;
    let head = deployment::head(tx, &environment)?;
    let clusters = deployment::cluster_domains(tx, &environment.summary.id, &head)?;
    let applied = &head.applied;
    // Containers run as Applied State has them, under the name the Service has now.
    let services = applied
        .services
        .iter()
        .filter_map(|applied| {
            let now = environment
                .working
                .services
                .iter()
                .find(|service| service.id == applied.id)
                .unwrap_or(applied);
            let name = ServiceName::parse(now.slug.as_str()).ok()?;
            Some((name, applied.config.private_dns.clone()))
        })
        .collect();
    let authored = environment
        .working
        .services
        .iter()
        .filter_map(|service| ServiceName::parse(service.slug.as_str()).ok())
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
