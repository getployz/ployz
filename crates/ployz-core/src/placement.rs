//! The deterministic placement pick a deploy runs over live bids.
//!
//! `pick_placement` is pure: the same inputs always produce the same targets
//! or refusal.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::corrosion::{ServicePlacement, ServiceReplicaCount};
use crate::ids::{DeployName, MachineName};
use crate::machine::MachineLifecycle;

/// Placement never targets a machine reporting less free disk than this
/// floor. It protects the machine's OS, Docker daemon, and Corrosion replica
/// from the disk pressure an image pull or container write layer would add
/// to an already-full disk.
pub const PLACEMENT_FREE_DISK_FLOOR_BYTES: u64 = 1024 * 1024 * 1024;

/// One machine's live input to a placement pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementBid {
    pub machine_name: MachineName,
    pub lifecycle: MachineLifecycle,
    pub endpoint_network_ready: bool,
    pub free_disk_bytes: u64,
    pub load: crate::corrosion::MachineLoadBand,
    pub total_container_count: usize,
    pub service_containers: Vec<ServiceContainerObservation>,
}

/// One local container fact used only to prefer the incumbent generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceContainerObservation {
    pub deploy: DeployName,
}

/// Everything the pick may consult. Gathering is the driver's job; the pick
/// itself performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementPickInputs {
    /// The service's effective placement: the request's, or the incumbent
    /// row's when the deploy carried none.
    pub placement: ServicePlacement,
    /// The effective pin set; empty means unpinned.
    pub pinned_machines: BTreeSet<MachineName>,
    /// Whether this first deploy declares any named volume mounts.
    pub has_named_volumes: bool,
    /// The incumbent's active deploy; `None` on a first deploy.
    pub active_deploy: Option<DeployName>,
    /// The live bids gathered from responding machines.
    pub bids: Vec<PlacementBid>,
}

/// One bidder dropped at tier zero, with the reason it can never host this
/// deploy's containers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementElimination {
    pub machine_name: MachineName,
    pub reason: PlacementEliminationReason,
}

/// Why a bidder was dropped before any preference tier ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlacementEliminationReason {
    Draining,
    EndpointNetworkUnavailable,
    FreeDiskBelowFloor { free_disk_bytes: u64 },
    OutsidePinSet,
}

/// A typed refusal to derive any target set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlacementRefusal {
    NoEligibleMachines {
        eliminations: Vec<PlacementElimination>,
    },
    VolumeReplicaLimit {
        requested: ServiceReplicaCount,
    },
}

/// Derives a deploy's target machine set from live bids.
///
/// Existing replicas keep their machines before eligibility is considered for
/// any shortfall. New replicas then use tier-0 drops (draining, free disk below
/// [`PLACEMENT_FREE_DISK_FLOOR_BYTES`], outside the pin set), followed by spread
/// (fewest total managed containers), then load band (idle before normal
/// before hot), then lexicographically lowest machine name. Replicas fill round-robin over the
/// tier-sorted survivors; stacking is allowed.
/// A global service keeps incumbent machines and targets every tier-0 survivor exactly once.
pub fn pick_placement(inputs: &PlacementPickInputs) -> Result<Vec<MachineName>, PlacementRefusal> {
    let PlacementPickInputs {
        placement,
        pinned_machines,
        has_named_volumes,
        active_deploy,
        bids,
    } = inputs;

    if *has_named_volumes
        && let ServicePlacement::Replicated { replicas } = placement
        && replicas.get() > 1
    {
        return Err(PlacementRefusal::VolumeReplicaLimit {
            requested: *replicas,
        });
    }

    let mut eliminations = Vec::new();
    let mut survivors = Vec::new();
    for bid in bids {
        match tier_zero_drop(bid, pinned_machines) {
            Some(reason) => eliminations.push(PlacementElimination {
                machine_name: bid.machine_name.clone(),
                reason,
            }),
            None => survivors.push(bid),
        }
    }
    match placement {
        ServicePlacement::Global { host_ports: _ } => {
            let mut targets = bids
                .iter()
                .filter(|bid| has_active_incumbent(bid, active_deploy.as_ref()))
                .map(|bid| bid.machine_name.clone())
                .collect::<BTreeSet<_>>();
            targets.extend(survivors.into_iter().map(|bid| bid.machine_name.clone()));
            if targets.is_empty() {
                Err(PlacementRefusal::NoEligibleMachines { eliminations })
            } else {
                Ok(targets.into_iter().collect())
            }
        }
        ServicePlacement::Replicated { replicas } => {
            let desired = usize::from(replicas.get());
            let mut targets = bids
                .iter()
                .flat_map(|bid| {
                    bid.service_containers
                        .iter()
                        .filter(|container| active_deploy.as_ref() == Some(&container.deploy))
                        .map(|_| bid.machine_name.clone())
                })
                .collect::<Vec<_>>();
            targets.sort();
            targets.truncate(desired);
            if targets.len() == desired {
                return Ok(targets);
            }
            if survivors.is_empty() {
                return Err(PlacementRefusal::NoEligibleMachines { eliminations });
            }
            survivors.sort_by_key(|bid| preference_key(bid));
            targets.extend(round_robin_fill(&survivors, desired - targets.len()));
            Ok(targets)
        }
    }
}

fn has_active_incumbent(bid: &PlacementBid, active_deploy: Option<&DeployName>) -> bool {
    bid.service_containers
        .iter()
        .any(|container| active_deploy == Some(&container.deploy))
}

fn tier_zero_drop(
    bid: &PlacementBid,
    pinned_machines: &BTreeSet<MachineName>,
) -> Option<PlacementEliminationReason> {
    match bid.lifecycle {
        MachineLifecycle::Draining => return Some(PlacementEliminationReason::Draining),
        MachineLifecycle::Active => {}
    }
    if !bid.endpoint_network_ready {
        return Some(PlacementEliminationReason::EndpointNetworkUnavailable);
    }
    if bid.free_disk_bytes < PLACEMENT_FREE_DISK_FLOOR_BYTES {
        return Some(PlacementEliminationReason::FreeDiskBelowFloor {
            free_disk_bytes: bid.free_disk_bytes,
        });
    }
    if !pinned_machines.is_empty() && !pinned_machines.contains(&bid.machine_name) {
        return Some(PlacementEliminationReason::OutsidePinSet);
    }
    None
}

/// New replicas prefer the fewest total containers, then the lowest load band,
/// then the lowest machine name.
fn preference_key(bid: &PlacementBid) -> (usize, crate::corrosion::MachineLoadBand, MachineName) {
    (
        bid.total_container_count,
        bid.load,
        bid.machine_name.clone(),
    )
}

fn round_robin_fill(survivors: &[&PlacementBid], replicas: usize) -> Vec<MachineName> {
    survivors
        .iter()
        .cycle()
        .take(replicas)
        .map(|bid| bid.machine_name.clone())
        .collect()
}
