//! Gateway serving route-table state.

use crate::roles::gateway::projection::{
    GatewayCertificateFailureAvailability, GatewayProjection, GatewayProjectionError,
    GatewayProjectionState, GatewayProjectionUpdate, apply_gateway_update,
};
#[cfg(test)]
use crate::roles::gateway::projection::{GatewayProjectedRoute, GatewayUpstream};
#[cfg(test)]
use ployz_core::operation::RouteTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjector {
    state: GatewayProjectionState,
    route_table: GatewayRouteTable,
}

impl GatewayProjector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: GatewayProjectionState::unavailable(),
            route_table: GatewayRouteTable::empty(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> &GatewayProjectionState {
        &self.state
    }

    #[must_use]
    #[cfg(test)]
    pub const fn route_table(&self) -> &GatewayRouteTable {
        &self.route_table
    }

    pub fn apply_source_update(&mut self, update: GatewayProjectionUpdate) -> GatewayProjectorTick {
        let previous = std::mem::replace(&mut self.state, GatewayProjectionState::unavailable());
        self.state = apply_gateway_update(previous, update);

        if let Some(projection) = projection_to_serve(&self.state) {
            self.route_table.replace(projection.clone());
        }

        GatewayProjectorTick {
            state: self.state.clone(),
            served: self.route_table.current().cloned(),
            serving: serving_state_from_projection_state(&self.state),
        }
    }
}

impl Default for GatewayProjector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectorTick {
    pub state: GatewayProjectionState,
    pub served: Option<GatewayProjection>,
    pub serving: GatewayServingState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayServingState {
    Current {
        route_count: usize,
    },
    LastKnownGood {
        route_count: usize,
        error: GatewayProjectionError,
    },
    Unavailable {
        error: Option<GatewayProjectionError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRouteTable {
    current: Option<GatewayProjection>,
}

impl GatewayRouteTable {
    #[must_use]
    pub const fn empty() -> Self {
        Self { current: None }
    }

    #[must_use]
    #[cfg(test)]
    pub const fn from_projection(projection: GatewayProjection) -> Self {
        Self {
            current: Some(projection),
        }
    }

    #[must_use]
    pub const fn current(&self) -> Option<&GatewayProjection> {
        self.current.as_ref()
    }

    #[must_use]
    #[cfg(test)]
    pub fn routes(&self) -> &[GatewayProjectedRoute] {
        self.current
            .as_ref()
            .map(|projection| projection.routes.as_slice())
            .unwrap_or(&[])
    }

    #[cfg(test)]
    pub fn select_upstream(
        &self,
        target: &RouteTarget,
    ) -> Result<GatewayUpstream, GatewayRouteSelectionError> {
        let projection = self
            .current
            .as_ref()
            .ok_or(GatewayRouteSelectionError::RouteTableUnavailable)?;
        let route = projection
            .routes
            .iter()
            .find(|route| &route.target == target)
            .ok_or_else(|| GatewayRouteSelectionError::NoRoute {
                target: target.clone(),
            })?;
        let upstream =
            route
                .upstreams
                .first()
                .ok_or_else(|| GatewayRouteSelectionError::NoUpstream {
                    target: target.clone(),
                })?;

        Ok(upstream.clone())
    }

    fn replace(&mut self, projection: GatewayProjection) {
        self.current = Some(projection);
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayRouteSelectionError {
    RouteTableUnavailable,
    NoRoute { target: RouteTarget },
    NoUpstream { target: RouteTarget },
}

fn projection_to_serve(state: &GatewayProjectionState) -> Option<&GatewayProjection> {
    state.last_good.as_ref()
}

fn serving_state_from_projection_state(state: &GatewayProjectionState) -> GatewayServingState {
    match (&state.last_good, &state.last_error) {
        (Some(projection), None) => GatewayServingState::Current {
            route_count: projection.routes.len(),
        },
        (
            Some(_),
            Some(
                error @ GatewayProjectionError::CertificateMaterial {
                    availability: GatewayCertificateFailureAvailability::Unavailable,
                    ..
                },
            ),
        ) => GatewayServingState::Unavailable {
            error: Some(error.clone()),
        },
        (Some(projection), Some(error)) => GatewayServingState::LastKnownGood {
            route_count: projection.routes.len(),
            error: error.clone(),
        },
        (None, Some(error)) => GatewayServingState::Unavailable {
            error: Some(error.clone()),
        },
        (None, None) => GatewayServingState::Unavailable { error: None },
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::ids::RouteBindingId;
    use ployz_core::ingress::CertificateOwner;

    use super::*;
    use crate::roles::gateway::projection::{
        GatewayCertificateMaterialFailure, GatewayCertificateMaterialFailureKind,
        GatewayProjectionInput,
    };

    #[test]
    fn missing_material_is_unavailable_until_a_complete_projection_has_served() {
        let mut projector = GatewayProjector::new();
        let failure = GatewayCertificateMaterialFailure {
            owner: CertificateOwner::RouteBinding {
                route_binding_id: RouteBindingId::try_new("route_app").expect("route id"),
            },
            kind: GatewayCertificateMaterialFailureKind::MissingOrInvalid,
            message: "certificate artifact missing".to_owned(),
        };

        let cold = projector.apply_source_update(update(vec![failure.clone()]));
        assert!(matches!(
            cold.serving,
            GatewayServingState::Unavailable { .. }
        ));
        let repeated = projector.apply_source_update(update(vec![failure.clone()]));
        assert!(matches!(
            repeated.serving,
            GatewayServingState::Unavailable { .. }
        ));

        let current = projector.apply_source_update(update(Vec::new()));
        assert!(matches!(
            current.serving,
            GatewayServingState::Current { .. }
        ));

        let retained = projector.apply_source_update(update(vec![failure]));
        assert!(matches!(
            retained.serving,
            GatewayServingState::LastKnownGood { .. }
        ));
    }

    fn update(
        certificate_failures: Vec<GatewayCertificateMaterialFailure>,
    ) -> GatewayProjectionUpdate {
        GatewayProjectionUpdate::SourceAvailable(Box::new(GatewayProjectionInput {
            certificate_bundles: Vec::new(),
            certificate_failures,
            challenges: Vec::new(),
            routes: Vec::new(),
            serving: Vec::new(),
            observed_machines: Vec::new(),
        }))
    }
}

#[cfg(test)]
#[path = "route_table_tests.rs"]
mod integration_tests;
