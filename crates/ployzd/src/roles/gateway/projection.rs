//! Complete Corrosion-row projection for public HTTP ingress.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use ployz_core::corrosion::{
    ClusterDocument, ContainerDocument, GatewayProjectionAggregateFailure,
    GatewayProjectionInputKind, GatewayRouteAvailability, GatewayRouteObservation,
    GatewayRouteProjectionFailure, GatewayRouteProjectionOutcome, GatewayRouteUnavailableReason,
    IngressMode, MachineDocument, RouteBindingDocument, ServiceDocument, StoredRow,
    read_named_roster_rows, read_rows,
};
use ployz_core::ids::{ClusterName, MachineName, RouteHostname};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::operation::RouteTarget;

/// The complete row reads that define one gateway snapshot.
#[derive(Debug)]
pub struct GatewayProjectionInput {
    pub cluster_id: ClusterName,
    pub cluster: Vec<StoredRow>,
    pub machines: Vec<StoredRow>,
    pub services: Vec<StoredRow>,
    pub route_bindings: Vec<StoredRow>,
    pub containers: Vec<StoredRow>,
}

/// One failure-isolated fold: serving state and diagnostic evidence originate
/// from the same complete input snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewayFold {
    pub projection: GatewayProjection,
    pub route_observations: Vec<GatewayRouteObservation>,
    pub aggregate_failures: Vec<GatewayProjectionAggregateFailure>,
}

/// One immutable routing view installed at the request boundary in one swap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewayProjection {
    pub routes: Vec<GatewayProjectedRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectedRoute {
    pub id: RouteHostname,
    pub origin: RouteBindingOrigin,
    pub target: RouteTarget,
    pub upstreams: Vec<GatewayUpstream>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayUpstream {
    /// Corrosion container keys are deterministic evidence, not authority IDs.
    pub container_key: String,
    pub machine_id: MachineName,
    pub address: SocketAddr,
}

/// Applies tolerant readers and canonical-name row validation before
/// joining each Route Binding independently to current serving rows.
#[must_use]
pub fn project_gateway_rows(input: GatewayProjectionInput) -> GatewayFold {
    let GatewayProjectionInput {
        cluster_id,
        cluster,
        machines,
        services,
        route_bindings,
        containers,
    } = input;
    let mut aggregate_failures = Vec::new();

    let cluster_report = read_rows::<ClusterDocument>(&cluster_id, cluster);
    let accepted_cluster = match cluster_report.accepted.as_slice() {
        [row] if row.source.key == cluster_id.as_str() && row.value.cluster_id == cluster_id => {
            Some(row.value.clone())
        }
        _ => None,
    };
    record_rejected(
        &mut aggregate_failures,
        GatewayProjectionInputKind::Cluster,
        cluster_report.skipped.len()
            + usize::from(accepted_cluster.is_none() && !cluster_report.accepted.is_empty()),
    );

    let accepted_machine_ids = if let Some(cluster) = accepted_cluster.as_ref() {
        let report = read_named_roster_rows::<MachineDocument>(cluster, machines);
        record_rejected(
            &mut aggregate_failures,
            GatewayProjectionInputKind::Machines,
            report.skipped.len(),
        );
        report
            .accepted
            .into_iter()
            .filter_map(|row| MachineName::try_new(row.source.key).ok())
            .collect::<BTreeSet<_>>()
    } else {
        record_rejected(
            &mut aggregate_failures,
            GatewayProjectionInputKind::Machines,
            machines.len(),
        );
        BTreeSet::new()
    };

    let service_report = read_rows::<ServiceDocument>(&cluster_id, services);
    record_rejected(
        &mut aggregate_failures,
        GatewayProjectionInputKind::Services,
        service_report.skipped.len(),
    );
    let services = service_report
        .accepted
        .into_iter()
        .map(|row| {
            (
                (row.value.namespace_id.clone(), row.value.name.clone()),
                row.value,
            )
        })
        .collect::<BTreeMap<_, _>>();

    let container_report = read_rows::<ContainerDocument>(&cluster_id, containers);
    record_rejected(
        &mut aggregate_failures,
        GatewayProjectionInputKind::Containers,
        container_report.skipped.len(),
    );

    let route_report = read_rows::<RouteBindingDocument>(&cluster_id, route_bindings.clone());
    record_rejected(
        &mut aggregate_failures,
        GatewayProjectionInputKind::RouteBindings,
        route_report.skipped.len(),
    );
    let mut valid_routes = route_report
        .accepted
        .into_iter()
        .filter_map(|row| {
            RouteHostname::try_new(row.source.key.clone())
                .ok()
                .map(|id| (id, row.value))
        })
        .collect::<Vec<_>>();
    valid_routes.sort_by(|left, right| left.0.cmp(&right.0));

    let mut projected_routes = Vec::new();
    let mut route_observations = Vec::with_capacity(valid_routes.len());
    for (id, route) in valid_routes {
        let target = RouteTarget::new(route.hostname.clone());
        if route.ingress_mode != IngressMode::Direct {
            route_observations.push(GatewayRouteObservation {
                hostname: route.hostname,
                outcome: GatewayRouteProjectionOutcome::Failed {
                    failure: GatewayRouteProjectionFailure::UnsupportedIngressMode {
                        ingress_mode: route.ingress_mode,
                    },
                },
            });
            continue;
        }

        let (upstreams, availability) = match services
            .get(&(route.namespace_id.clone(), route.service_name.clone()))
        {
            None => (
                Vec::new(),
                GatewayRouteAvailability::Unavailable {
                    reason: GatewayRouteUnavailableReason::ServiceMissing,
                },
            ),
            Some(service) => {
                let mut upstreams = container_report
                    .accepted
                    .iter()
                    .filter(|container| {
                        accepted_machine_ids.contains(&container.value.machine_id)
                            && container.value.namespace_id == route.namespace_id
                            && container.value.service_name == route.service_name
                            && container.value.deploy == service.active_deploy
                    })
                    .map(|container| GatewayUpstream {
                        container_key: container.source.key.clone(),
                        machine_id: container.value.machine_id.clone(),
                        address: SocketAddr::from((container.value.ip, route.endpoint_port.get())),
                    })
                    .collect::<Vec<_>>();
                upstreams.sort_by(|left, right| {
                    left.machine_id
                        .cmp(&right.machine_id)
                        .then_with(|| left.container_key.cmp(&right.container_key))
                        .then_with(|| left.address.cmp(&right.address))
                });
                let availability = if upstreams.is_empty() {
                    GatewayRouteAvailability::Unavailable {
                        reason: GatewayRouteUnavailableReason::NoUpstream,
                    }
                } else {
                    GatewayRouteAvailability::Serving {
                        upstream_count: upstreams.len(),
                    }
                };
                (upstreams, availability)
            }
        };
        projected_routes.push(GatewayProjectedRoute {
            id: id.clone(),
            origin: route.origin,
            target,
            upstreams,
        });
        route_observations.push(GatewayRouteObservation {
            hostname: route.hostname,
            outcome: GatewayRouteProjectionOutcome::Applied { availability },
        });
    }
    projected_routes.sort_by(|left, right| left.target.cmp(&right.target));

    GatewayFold {
        projection: GatewayProjection {
            routes: projected_routes,
        },
        route_observations,
        aggregate_failures,
    }
}

fn record_rejected(
    failures: &mut Vec<GatewayProjectionAggregateFailure>,
    input: GatewayProjectionInputKind,
    rejected_rows: usize,
) {
    if rejected_rows > 0 {
        failures.push(GatewayProjectionAggregateFailure {
            input,
            rejected_rows,
        });
    }
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
