//! Gateway runtime state.

use crate::gateway::{
    GatewayProjectedRoute, GatewayProjection, GatewayProjectionError, GatewayProjectionState,
    GatewayProjectionUpdate, GatewayUpstream, apply_gateway_update,
};
use crate::gateway_source::load_gateway_projection_update_from_nats;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRuntime {
    state: GatewayProjectionState,
    route_table: GatewayRouteTable,
}

impl GatewayRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: GatewayProjectionState::Unavailable,
            route_table: GatewayRouteTable::empty(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> &GatewayProjectionState {
        &self.state
    }

    #[must_use]
    pub const fn route_table(&self) -> &GatewayRouteTable {
        &self.route_table
    }

    pub fn apply_source_update(&mut self, update: GatewayProjectionUpdate) -> GatewayRuntimeTick {
        let previous = std::mem::replace(&mut self.state, GatewayProjectionState::Unavailable);
        self.state = apply_gateway_update(previous, update);

        if let Some(projection) = projection_to_serve(&self.state) {
            self.route_table.replace(projection.clone());
        }

        GatewayRuntimeTick {
            state: self.state.clone(),
            served: self.route_table.current().cloned(),
            serving: serving_state_from_projection_state(&self.state),
        }
    }
}

impl Default for GatewayRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRuntimeTick {
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
    pub fn routes(&self) -> &[GatewayProjectedRoute] {
        self.current
            .as_ref()
            .map(|projection| projection.routes.as_slice())
            .unwrap_or(&[])
    }

    pub fn select_upstream(
        &self,
        target: &ployz_core::ops::RouteTarget,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayRouteSelectionError {
    RouteTableUnavailable,
    NoRoute {
        target: ployz_core::ops::RouteTarget,
    },
    NoUpstream {
        target: ployz_core::ops::RouteTarget,
    },
}

pub async fn refresh_gateway_runtime_from_nats(
    runtime: &mut GatewayRuntime,
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
) -> GatewayRuntimeTick {
    let update = load_gateway_projection_update_from_nats(core_state, observations).await;
    runtime.apply_source_update(update)
}

fn projection_to_serve(state: &GatewayProjectionState) -> Option<&GatewayProjection> {
    match state {
        GatewayProjectionState::Current(projection)
        | GatewayProjectionState::LastKnownGood(projection)
        | GatewayProjectionState::ProjectionFailedRetained {
            retained: projection,
            ..
        } => Some(projection),
        GatewayProjectionState::ProjectionFailedUnavailable { .. }
        | GatewayProjectionState::Unavailable => None,
    }
}

fn serving_state_from_projection_state(state: &GatewayProjectionState) -> GatewayServingState {
    match state {
        GatewayProjectionState::Current(projection) => GatewayServingState::Current {
            route_count: projection.routes.len(),
        },
        GatewayProjectionState::LastKnownGood(projection) => GatewayServingState::LastKnownGood {
            route_count: projection.routes.len(),
            error: GatewayProjectionError::SourceUnavailable {
                message: "gateway source unavailable".to_owned(),
            },
        },
        GatewayProjectionState::ProjectionFailedRetained { retained, error } => {
            GatewayServingState::LastKnownGood {
                route_count: retained.routes.len(),
                error: error.clone(),
            }
        }
        GatewayProjectionState::ProjectionFailedUnavailable { error } => {
            GatewayServingState::Unavailable {
                error: Some(error.clone()),
            }
        }
        GatewayProjectionState::Unavailable => GatewayServingState::Unavailable { error: None },
    }
}
