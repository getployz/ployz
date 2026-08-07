//! The deterministic, replayable placement pick a deploy runs over live bids.
//!
//! `pick_placement` is pure: the same inputs always produce the same targets,
//! the same eliminations, and the same refusals, so an operation's evidence
//! replays the pick exactly.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::api::v2::PlacementBid;
use crate::corrosion::{ServicePlacement, ServiceReplicaCount};
use crate::deploy::VolumeName;
use crate::ids::{MachineRowId, OperationRowId};
use crate::machine::{MachineLifecycle, MachineName};

/// Placement never targets a machine reporting less free disk than this
/// floor. It protects the machine's OS, Docker daemon, and Corrosion replica
/// from the disk pressure an image pull or container write layer would add
/// to an already-full disk.
pub const PLACEMENT_FREE_DISK_FLOOR_BYTES: u64 = 1024 * 1024 * 1024;

/// Everything the pick may consult. Gathering is the driver's job; the pick
/// itself performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementPickInputs {
    /// The service's effective placement: the request's, or the incumbent
    /// row's when the deploy carried none.
    pub placement: ServicePlacement,
    /// The effective pin set; empty means unpinned.
    pub pinned_machines: BTreeSet<MachineRowId>,
    /// The service's declared named volumes.
    pub volumes: BTreeSet<VolumeName>,
    /// Machines that plausibly hold this service's volumes: machines named
    /// by existing container rows plus the pin set.
    pub plausible_volume_holders: BTreeSet<MachineRowId>,
    /// The incumbent's active deploy; `None` on a first deploy.
    pub active_deploy: Option<OperationRowId>,
    /// The live bids gathered from responding machines.
    pub bids: Vec<PlacementBid>,
    /// Roster machines that yielded no bid, keyed by row id with the human
    /// name a refusal renders.
    pub silent_machines: BTreeMap<MachineRowId, MachineName>,
}

/// The pick's replayable outcome: an ordered target list (a machine appears
/// once per replica it hosts), every elimination, and any loud shortfall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementPick {
    pub targets: Vec<MachineRowId>,
    pub eliminations: Vec<PlacementElimination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortfall: Option<PlacementShortfall>,
}

/// One machine named by a placement outcome, carrying the human name the
/// `--machine` flag accepts alongside the row id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementMachine {
    pub machine_id: MachineRowId,
    pub machine_name: MachineName,
}

/// One bidder dropped at tier zero, with the reason it can never host this
/// deploy's containers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementElimination {
    pub machine_id: MachineRowId,
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
    VolumeNotHeld { holder: PlacementMachine },
    OutsidePinSet,
}

/// Fewer distinct machines than requested replicas: every replica still
/// placed by stacking, recorded loudly instead of refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementShortfall {
    pub requested: ServiceReplicaCount,
    /// Distinct machines the replicas landed on.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub placed: usize,
}

/// A typed refusal to derive any target set. Zero eligible bidders is the
/// only capacity refusal; the volume refusals keep a data fork or a dark
/// holder in front of a human instead of silently recreating data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlacementRefusal {
    NoEligibleMachines {
        eliminations: Vec<PlacementElimination>,
    },
    VolumeHolderConflict {
        volume: VolumeName,
        holders: Vec<PlacementMachine>,
    },
    DarkVolumeHolder {
        machines: Vec<PlacementMachine>,
    },
    VolumeReplicaLimit {
        requested: ServiceReplicaCount,
    },
}

/// Derives a deploy's target machine set from live bids.
///
/// Tier order: tier-0 drops (draining, free disk below
/// [`PLACEMENT_FREE_DISK_FLOOR_BYTES`], outside the pin set, not the volume
/// holder), then sticky (runs the incumbent's active deploy), then spread
/// (fewest total managed containers), then load band (idle before normal
/// before hot), then lowest machine ULID. Replicas fill round-robin over the
/// tier-sorted survivors; stacking is allowed and recorded as a shortfall.
/// A global service targets every tier-0 survivor exactly once, and volume
/// holder rules never fire for it: each machine mounts its own local volume.
pub fn pick_placement(inputs: &PlacementPickInputs) -> Result<PlacementPick, PlacementRefusal> {
    let PlacementPickInputs {
        placement,
        pinned_machines,
        volumes,
        plausible_volume_holders,
        active_deploy,
        bids,
        silent_machines,
    } = inputs;

    let held_volumes = match placement {
        ServicePlacement::Replicated { replicas } => adjudicate_volumes(
            *replicas,
            volumes,
            pinned_machines,
            plausible_volume_holders,
            bids,
            silent_machines,
        )?,
        ServicePlacement::Global { host_ports: _ } => BTreeMap::new(),
    };

    let mut eliminations = Vec::new();
    let mut survivors = Vec::new();
    for bid in bids {
        match tier_zero_drop(bid, pinned_machines, &held_volumes) {
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
            Ok(PlacementPick {
                targets: survivors
                    .into_iter()
                    .map(|bid| bid.machine_id.clone())
                    .collect(),
                eliminations,
                shortfall: None,
            })
        }
        ServicePlacement::Replicated { replicas } => {
            survivors.sort_by_key(|bid| preference_key(bid, active_deploy.as_ref()));
            let shortfall =
                (usize::from(replicas.get()) > survivors.len()).then_some(PlacementShortfall {
                    requested: *replicas,
                    placed: survivors.len(),
                });
            Ok(PlacementPick {
                targets: round_robin_fill(&survivors, *replicas),
                eliminations,
                shortfall,
            })
        }
    }
}

/// Resolves the volume affinity rules for a replicated service. Returns each
/// held declared volume's single visible holder; an empty map means the pick
/// runs normally (no volumes, or a first deploy with no holder anywhere).
/// Tier zero then requires a target to hold every held volume, so a
/// multi-volume split across machines eliminates every bidder instead of
/// inventing a fork.
fn adjudicate_volumes(
    replicas: ServiceReplicaCount,
    volumes: &BTreeSet<VolumeName>,
    pinned_machines: &BTreeSet<MachineRowId>,
    plausible_volume_holders: &BTreeSet<MachineRowId>,
    bids: &[PlacementBid],
    silent_machines: &BTreeMap<MachineRowId, MachineName>,
) -> Result<BTreeMap<VolumeName, PlacementMachine>, PlacementRefusal> {
    if volumes.is_empty() {
        return Ok(BTreeMap::new());
    }
    if replicas.get() > 1 {
        return Err(PlacementRefusal::VolumeReplicaLimit {
            requested: replicas,
        });
    }
    // A silent plausible holder wins over every visible holder — and over
    // any pin: the dark machine may hold a fork of the data, and a fork
    // deserves a human.
    let dark_holders = plausible_volume_holders
        .iter()
        .filter_map(|machine_id| {
            silent_machines
                .get(machine_id)
                .map(|name| PlacementMachine {
                    machine_id: machine_id.clone(),
                    machine_name: name.clone(),
                })
        })
        .collect::<Vec<_>>();
    if !dark_holders.is_empty() {
        return Err(PlacementRefusal::DarkVolumeHolder {
            machines: dark_holders,
        });
    }
    // Holders outside a non-empty pin set are OutsidePinSet drops at tier
    // zero, so counting them here would refuse a fork the pin already
    // resolves; `--machine <holder>` must remain the conflict's resolver.
    let mut holders_by_volume: BTreeMap<VolumeName, Vec<PlacementMachine>> = BTreeMap::new();
    for bid in bids {
        if !pinned_machines.is_empty() && !pinned_machines.contains(&bid.machine_id) {
            continue;
        }
        for volume in bid.volumes_held.intersection(volumes) {
            holders_by_volume
                .entry(volume.clone())
                .or_default()
                .push(PlacementMachine {
                    machine_id: bid.machine_id.clone(),
                    machine_name: bid.machine_name.clone(),
                });
        }
    }
    // Two visible holders of the same volume are a data fork: refuse,
    // naming that volume and exactly its holders.
    if let Some((volume, holders)) = holders_by_volume
        .iter()
        .find(|(_, holders)| holders.len() > 1)
    {
        return Err(PlacementRefusal::VolumeHolderConflict {
            volume: volume.clone(),
            holders: holders.clone(),
        });
    }
    let mut held = BTreeMap::new();
    for (volume, holders) in holders_by_volume {
        let [holder] = holders.as_slice() else {
            continue;
        };
        held.insert(volume, holder.clone());
    }
    Ok(held)
}

fn tier_zero_drop(
    bid: &PlacementBid,
    pinned_machines: &BTreeSet<MachineRowId>,
    held_volumes: &BTreeMap<VolumeName, PlacementMachine>,
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
    if let Some((_, holder)) = held_volumes
        .iter()
        .find(|(_, holder)| holder.machine_id != bid.machine_id)
    {
        return Some(PlacementEliminationReason::VolumeNotHeld {
            holder: holder.clone(),
        });
    }
    None
}

/// The replicated preference key: sticky bids first, then fewest total
/// containers, then the lowest load band, then the lowest machine ULID.
fn preference_key(
    bid: &PlacementBid,
    active_deploy: Option<&OperationRowId>,
) -> (bool, usize, crate::corrosion::MachineLoadBand, MachineRowId) {
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
) -> Vec<MachineRowId> {
    survivors
        .iter()
        .cycle()
        .take(usize::from(replicas.get()))
        .map(|bid| bid.machine_id.clone())
        .collect()
}
