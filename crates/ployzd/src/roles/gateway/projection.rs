//! Complete Corrosion-row projection for public HTTP ingress.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use ployz_core::corrosion::{
    ContainerDocument, IngressMode, RouteBindingDocument, ServiceDocument, StoredRow,
    read_named_rows, read_rows,
};
use ployz_core::ids::{ClusterId, MachineRowId, RouteBindingRowId, ServiceRowId};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::operation::RouteTarget;

/// The three complete table reads that define one gateway snapshot.
#[derive(Debug)]
pub struct GatewayProjectionInput {
    pub cluster_id: ClusterId,
    pub services: Vec<StoredRow>,
    pub route_bindings: Vec<StoredRow>,
    pub containers: Vec<StoredRow>,
}

/// One immutable routing view installed at the request boundary in one swap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GatewayProjection {
    pub routes: Vec<GatewayProjectedRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectedRoute {
    pub id: RouteBindingRowId,
    pub origin: RouteBindingOrigin,
    pub target: RouteTarget,
    pub upstreams: Vec<GatewayUpstream>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayUpstream {
    /// Corrosion container keys are deterministic evidence, not authority IDs.
    pub container_key: String,
    pub machine_id: MachineRowId,
    pub address: SocketAddr,
}

/// Applies tolerant readers and lowest-ULID named-row adjudication before
/// joining an exact Route Binding to the service's current serving decision.
#[must_use]
pub fn project_gateway_rows(input: GatewayProjectionInput) -> GatewayProjection {
    let GatewayProjectionInput {
        cluster_id,
        services,
        route_bindings,
        containers,
    } = input;

    let services = read_named_rows::<ServiceDocument>(&cluster_id, services)
        .accepted
        .into_iter()
        .filter_map(|row| {
            ServiceRowId::try_new(row.id.as_str().to_owned())
                .ok()
                .map(|id| (id, row.value))
        })
        .collect::<BTreeMap<_, _>>();
    let containers = read_rows::<ContainerDocument>(&cluster_id, containers).accepted;
    let mut routes = Vec::new();

    for row in read_named_rows::<RouteBindingDocument>(&cluster_id, route_bindings).accepted {
        let route = row.value;
        if route.ingress_mode != IngressMode::Direct {
            continue;
        }
        let Ok(id) = RouteBindingRowId::try_new(row.id.as_str().to_owned()) else {
            continue;
        };
        let mut upstreams = Vec::new();
        if let Some(service) = services.get(&route.service_id)
            && service.namespace_id == route.namespace_id
        {
            for container in &containers {
                if container.value.namespace_id == route.namespace_id
                    && container.value.service_id == route.service_id
                    && container.value.deploy == service.active_deploy
                {
                    upstreams.push(GatewayUpstream {
                        container_key: container.source.key.clone(),
                        machine_id: container.value.machine_id.clone(),
                        address: SocketAddr::from((container.value.ip, route.endpoint_port.get())),
                    });
                }
            }
        }
        upstreams.sort_by(|left, right| {
            left.machine_id
                .cmp(&right.machine_id)
                .then_with(|| left.container_key.cmp(&right.container_key))
                .then_with(|| left.address.cmp(&right.address))
        });
        routes.push(GatewayProjectedRoute {
            id,
            origin: route.origin,
            target: RouteTarget::new(route.hostname),
            upstreams,
        });
    }
    routes.sort_by(|left, right| left.target.cmp(&right.target));
    GatewayProjection { routes }
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
