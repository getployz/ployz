//! Gateway runtime state.

use crate::gateway::{
    GatewayProjectedRoute, GatewayProjection, GatewayProjectionState, GatewayProjectionUpdate,
    apply_gateway_update,
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

    fn replace(&mut self, projection: GatewayProjection) {
        self.current = Some(projection);
    }
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
