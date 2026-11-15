//! Which copies of a Service a proxy can reach: the Machines membership reports Down and the
//! route kept from the last pass, shared by Internal DNS and the Ingress Proxy so both read one
//! fallback.

use std::collections::BTreeSet;

use ployz_core::{MachineId, ProxyRoutes, ServiceContainer, proxy_routes, reachable_routes};

use crate::corrosion::Error as CorrosionError;

/// One pass's routes: the route a proxy carries and acks, and the copies a client can reach now.
pub(crate) struct ReachPass {
    /// Every copy [`proxy_routes`] keeps, which the next pass carries.
    pub(crate) routes: ProxyRoutes,
    /// `routes` without the copies seen stopped or withdrawn, for what a proxy dials or answers.
    pub(crate) reachable: ProxyRoutes,
}

/// The Down Machines and the route a proxy carries between passes.
pub(crate) struct Reach {
    down: DownMachines,
    /// Kept while no copy of a Service is reachable.
    routed: ProxyRoutes,
}

impl Reach {
    pub(crate) fn new(local_id: MachineId) -> Self {
        Self {
            down: DownMachines::new(local_id),
            routed: ProxyRoutes::new(),
        }
    }

    pub(crate) fn local_id(&self) -> MachineId {
        self.down.local_id
    }

    /// Whether the Down set changed, so the routes may have.
    pub(crate) fn update_down(
        &mut self,
        loaded: Result<BTreeSet<MachineId>, CorrosionError>,
    ) -> bool {
        self.down.update(loaded)
    }

    #[cfg(test)]
    pub(crate) fn is_down(&self, machine: &MachineId) -> bool {
        self.down.contains(machine)
    }

    /// The routes the observed `containers` give, carrying the route kept by [`Self::keep`].
    pub(crate) fn pass(&self, containers: &[ServiceContainer]) -> ReachPass {
        let is_down = |machine: &MachineId| self.down.contains(machine);
        let routes = proxy_routes(containers, is_down, &self.routed);
        let reachable = reachable_routes(
            &routes,
            containers.iter().map(ServiceContainer::as_observation),
            is_down,
        );
        ReachPass { routes, reachable }
    }

    /// Carry `routes` into the next pass.
    pub(crate) fn keep(&mut self, routes: ProxyRoutes) {
        self.routed = routes;
    }
}

struct DownMachines {
    local_id: MachineId,
    down: Option<BTreeSet<MachineId>>,
}

impl DownMachines {
    fn new(local_id: MachineId) -> Self {
        Self {
            local_id,
            down: None,
        }
    }

    fn contains(&self, machine: &MachineId) -> bool {
        *machine != self.local_id
            && self
                .down
                .as_ref()
                .is_some_and(|down| down.contains(machine))
    }

    fn update(&mut self, loaded: Result<BTreeSet<MachineId>, CorrosionError>) -> bool {
        match loaded {
            Ok(down) if self.down.as_ref() == Some(&down) => false,
            Ok(down) => {
                self.down = Some(down);
                true
            }
            // A failed sample is unknown evidence, as `ReadRoutes` and the LastIn
            // guard read it, so no Machine stays filtered as Down on a stale set.
            Err(error) => {
                eprintln!(
                    "failed to load membership for routing; routing every Machine: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                self.down.take().is_some_and(|down| !down.is_empty())
            }
        }
    }
}

#[cfg(test)]
#[path = "reach_tests.rs"]
mod tests;
