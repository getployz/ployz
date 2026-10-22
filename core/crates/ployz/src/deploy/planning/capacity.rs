//! Endpoint-capacity filtering and plan-wide budgeting.

use std::collections::BTreeMap;

use ployz_core::{BridgeEndpointCapacity, MachineId};

use super::{DeploySnapshot, PlanError};

pub(crate) fn endpoint_capacity_error(
    required: usize,
    capacity: Option<&BridgeEndpointCapacity>,
) -> Option<PlanError> {
    match capacity {
        None if required > 0 => Some(PlanError::CapacityUnknown),
        Some(capacity) if capacity.free_endpoints() < required as u64 => {
            Some(PlanError::InsufficientCapacity)
        }
        None | Some(_) => None,
    }
}

#[derive(Clone, Copy)]
pub(super) struct EndpointDemand {
    peak: u64,
    persistent: u64,
    uses_hook: bool,
}

#[derive(Clone, Copy)]
pub(super) enum EndpointOperation {
    Unchanged,
    /// The running Container stays and takes new limits in place.
    Update,
    Create,
    Replace,
}

impl EndpointOperation {
    /// Whether the current Container keeps running under its own endpoint and ports.
    pub(super) const fn keeps_container(self) -> bool {
        matches!(self, Self::Unchanged | Self::Update)
    }
}

impl EndpointDemand {
    pub(super) fn for_operation(operation: EndpointOperation, hook_pending: bool) -> Self {
        let changes = !operation.keeps_container();
        let uses_hook = changes && hook_pending;
        Self {
            peak: u64::from(changes) + u64::from(uses_hook),
            persistent: u64::from(matches!(operation, EndpointOperation::Create))
                + u64::from(uses_hook),
            uses_hook,
        }
    }

    pub(super) fn uses_hook(self) -> bool {
        self.uses_hook
    }
}

/// Free endpoints per Machine. A count goes negative when a reservation's credit for an
/// old hook is taken back after the endpoints it freed were spent: the Machine is in
/// debt, and later refunds pay the debt before they free anything.
#[derive(Clone)]
pub(super) struct CapacityBudget {
    free: Option<BTreeMap<MachineId, i64>>,
}

impl CapacityBudget {
    pub(super) fn from_snapshot(snapshot: &DeploySnapshot) -> Self {
        Self {
            free: snapshot.capacity.as_ref().map(|capacity| {
                capacity
                    .iter()
                    .map(|(id, capacity)| {
                        (
                            *id,
                            i64::try_from(capacity.free_endpoints()).unwrap_or(i64::MAX),
                        )
                    })
                    .collect()
            }),
        }
    }

    pub(super) fn fits(&self, machine_id: &MachineId, peak: u64) -> bool {
        peak == 0
            || self.free.as_ref().is_none_or(|free| {
                free.get(machine_id)
                    .is_some_and(|free| u64::try_from(*free).is_ok_and(|free| free >= peak))
            })
    }

    pub(super) fn fits_demand(&self, machine_id: &MachineId, demand: EndpointDemand) -> bool {
        self.fits(machine_id, demand.peak)
    }

    pub(super) fn reserve(&mut self, machine_id: &MachineId, demand: EndpointDemand) -> bool {
        if !self.fits_demand(machine_id, demand) {
            return false;
        }
        if demand.persistent > 0
            && let Some(free) = self.free.as_mut()
        {
            *free
                .get_mut(machine_id)
                .expect("capacity candidates have fresh telemetry") -=
                i64::try_from(demand.persistent)
                    .expect("a Service demands at most a few endpoints");
        }
        true
    }

    pub(super) fn release(&mut self, machine_id: &MachineId) {
        if let Some(free) = self.free.as_mut().and_then(|free| free.get_mut(machine_id)) {
            *free = free.saturating_add(1);
        }
    }

    /// Undo one `release`: an endpoint of `machine_id` stays in use after all.
    pub(super) fn take(&mut self, machine_id: &MachineId) {
        if let Some(free) = self.free.as_mut().and_then(|free| free.get_mut(machine_id)) {
            *free = free.saturating_sub(1);
        }
    }

    pub(super) fn error_for<'a>(
        &self,
        machine_ids: impl IntoIterator<Item = &'a MachineId>,
    ) -> PlanError {
        if self.free.as_ref().is_some_and(|free| {
            machine_ids
                .into_iter()
                .any(|machine_id| !free.contains_key(machine_id))
        }) {
            PlanError::CapacityUnknown
        } else {
            PlanError::InsufficientCapacity
        }
    }

    pub(super) fn can_supply_persistent<'a>(
        &self,
        machine_ids: impl IntoIterator<Item = &'a MachineId>,
        required: usize,
    ) -> bool {
        if required == 0 {
            return true;
        }
        let Some(free) = &self.free else {
            return true;
        };
        let required = u64::try_from(required).unwrap_or(u64::MAX);
        let mut available = 0_u64;
        for machine_id in machine_ids {
            let Some(machine_free) = free.get(machine_id) else {
                continue;
            };
            available = available.saturating_add(u64::try_from(*machine_free).unwrap_or(0));
            if available >= required {
                return true;
            }
        }
        false
    }
}
