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
    pub machine_id: MachineName,
    pub machine_name: MachineName,
    pub lifecycle: MachineLifecycle,
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
    pub machine_id: MachineName,
    pub machine_name: MachineName,
    pub reason: PlacementEliminationReason,
}

/// Why a bidder was dropped before any preference tier ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlacementEliminationReason {
    Draining,
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
/// Tier order: tier-0 drops (draining, free disk below
/// [`PLACEMENT_FREE_DISK_FLOOR_BYTES`], outside the pin set), then sticky
/// (runs the incumbent's active deploy), then spread
/// (fewest total managed containers), then load band (idle before normal
/// before hot), then lexicographically lowest machine name. Replicas fill round-robin over the
/// tier-sorted survivors; stacking is allowed.
/// A global service targets every tier-0 survivor exactly once.
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
                machine_id: bid.machine_id.clone(),
                machine_name: bid.machine_name.clone(),
                reason,
            }),
            None => survivors.push(bid),
        }
    }
    if survivors.is_empty() {
        return Err(PlacementRefusal::NoEligibleMachines { eliminations });
    }

    match placement {
        ServicePlacement::Global { host_ports: _ } => {
            survivors.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
            Ok(survivors
                .into_iter()
                .map(|bid| bid.machine_id.clone())
                .collect())
        }
        ServicePlacement::Replicated { replicas } => {
            survivors.sort_by_key(|bid| preference_key(bid, active_deploy.as_ref()));
            Ok(round_robin_fill(&survivors, *replicas))
        }
    }
}

fn tier_zero_drop(
    bid: &PlacementBid,
    pinned_machines: &BTreeSet<MachineName>,
) -> Option<PlacementEliminationReason> {
    match bid.lifecycle {
        MachineLifecycle::Draining => return Some(PlacementEliminationReason::Draining),
        MachineLifecycle::Active => {}
    }
    if bid.free_disk_bytes < PLACEMENT_FREE_DISK_FLOOR_BYTES {
        return Some(PlacementEliminationReason::FreeDiskBelowFloor {
            free_disk_bytes: bid.free_disk_bytes,
        });
    }
    if !pinned_machines.is_empty() && !pinned_machines.contains(&bid.machine_id) {
        return Some(PlacementEliminationReason::OutsidePinSet);
    }
    None
}

/// The replicated preference key: sticky bids first, then fewest total
/// containers, then the lowest load band, then the lowest machine name.
fn preference_key(
    bid: &PlacementBid,
    active_deploy: Option<&DeployName>,
) -> (bool, usize, crate::corrosion::MachineLoadBand, MachineName) {
    let sticky = active_deploy.is_some_and(|active| {
        bid.service_containers
            .iter()
            .any(|container| &container.deploy == active)
    });
    (
        !sticky,
        bid.total_container_count,
        bid.load,
        bid.machine_id.clone(),
    )
}

fn round_robin_fill(
    survivors: &[&PlacementBid],
    replicas: ServiceReplicaCount,
) -> Vec<MachineName> {
    survivors
        .iter()
        .cycle()
        .take(usize::from(replicas.get()))
        .map(|bid| bid.machine_id.clone())
        .collect()
}
