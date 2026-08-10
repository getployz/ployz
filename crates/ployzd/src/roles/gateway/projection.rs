//! Complete Corrosion-row projection for public HTTP ingress.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use ployz_core::corrosion::{
    ClusterDocument, CorrosionTimestamp, GatewayProjectionAggregateFailure,
    GatewayProjectionInputKind, GatewayRouteAvailability, GatewayRouteObservation,
    GatewayRouteProjectionFailure, GatewayRouteProjectionOutcome, GatewayRouteUnavailableReason,
    IngressMode, MachineDocument, MachineEndpointDocument, NamespaceDocument, RouteBindingDocument,
    StoredRow, read_named_roster_rows, read_named_rows, read_rows, service_endpoint_key,
};
use ployz_core::ids::{ClusterName, MachineName, RouteHostname};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::operation::RouteTarget;

/// The complete row reads that define one gateway snapshot.
#[derive(Debug)]
pub struct GatewayProjectionInput {
    pub now: CorrosionTimestamp,
    pub cluster_id: ClusterName,
    pub cluster: Vec<StoredRow>,
    pub machines: Vec<StoredRow>,
    pub namespaces: Vec<StoredRow>,
    pub route_bindings: Vec<StoredRow>,
    pub machine_endpoints: Vec<StoredRow>,
}

/// One failure-isolated fold: serving state and diagnostic evidence originate
/// from the same complete input snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewayFold {
    pub projection: GatewayProjection,
    pub route_observations: Vec<GatewayRouteObservation>,
    pub aggregate_failures: Vec<GatewayProjectionAggregateFailure>,
    /// The next instant at which an installed upstream must be reprojected
    /// even if Corrosion produces no invalidation.
    pub next_endpoint_expiry: Option<std::time::Duration>,
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
    /// Natural endpoint identity used only to keep projection ordering stable.
    pub endpoint_key: String,
    pub machine_id: MachineName,
    pub address: SocketAddr,
}

/// Applies tolerant readers and canonical-name row validation before
/// joining each Route Binding independently to current serving rows.
#[must_use]
pub fn project_gateway_rows(input: GatewayProjectionInput) -> GatewayFold {
    let GatewayProjectionInput {
        now,
        cluster_id,
        cluster,
        machines,
        namespaces,
        route_bindings,
        machine_endpoints,
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

    let namespace_report = read_named_rows::<NamespaceDocument>(&cluster_id, namespaces);
    record_rejected(
        &mut aggregate_failures,
        GatewayProjectionInputKind::Namespaces,
        namespace_report.skipped.len(),
    );
    let namespaces = namespace_report
        .accepted
        .into_iter()
        .filter_map(|row| {
            ployz_core::ids::CorrosionNamespaceName::try_new(row.source.key)
                .ok()
                .map(|id| (id, row.value))
        })
        .collect::<BTreeMap<_, _>>();

    let endpoint_report = read_rows::<MachineEndpointDocument>(&cluster_id, machine_endpoints);
    record_rejected(
        &mut aggregate_failures,
        GatewayProjectionInputKind::MachineEndpoints,
        endpoint_report.skipped.len(),
    );
    let serving_endpoint_rows = endpoint_report
        .accepted
        .iter()
        .filter_map(|row| {
            if !accepted_machine_ids.contains(&row.value.machine_id) {
                return None;
            }
            row.value
                .serving_freshness_remaining(now)
                .map(|remaining| (row, remaining))
        })
        .collect::<Vec<_>>();
    let next_endpoint_expiry = serving_endpoint_rows
        .iter()
        .filter(|(row, _)| !row.value.endpoints.is_empty())
        .map(|(_, remaining)| *remaining)
        .min();

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

        let service = namespaces
            .get(&route.namespace_id)
            .and_then(|namespace| namespace.services.get(&route.service_name));
        let (upstreams, availability) = match service {
            None => (
                Vec::new(),
                GatewayRouteAvailability::Unavailable {
                    reason: GatewayRouteUnavailableReason::ServiceMissing,
                },
            ),
            Some(service) => {
                let mut upstreams = serving_endpoint_rows
                    .iter()
                    .flat_map(|(testimony, _)| {
                        testimony
                            .value
                            .endpoints
                            .iter()
                            .filter(|endpoint| {
                                endpoint.namespace_id == route.namespace_id
                                    && endpoint.service_name == route.service_name
                                    && endpoint.deploy == service.active_deploy
                            })
                            .map(|endpoint| GatewayUpstream {
                                endpoint_key: service_endpoint_key(
                                    endpoint,
                                    &testimony.value.machine_id,
                                ),
                                machine_id: testimony.value.machine_id.clone(),
                                address: SocketAddr::from((endpoint.ip, route.endpoint_port.get())),
                            })
                    })
                    .collect::<Vec<_>>();
                upstreams.sort_by(|left, right| {
                    left.machine_id
                        .cmp(&right.machine_id)
                        .then_with(|| left.endpoint_key.cmp(&right.endpoint_key))
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
        next_endpoint_expiry,
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
