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
use crate::image::OciPlatform;
use crate::machine::MachineLifecycle;

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
    /// Platforms the resolved image supports. `None` when the manifest did
    /// not declare platforms; the platform drop is skipped and a wrong-arch
    /// machine fails loudly at pull instead.
    pub platforms: Option<BTreeSet<OciPlatform>>,
    /// The live bids gathered from responding machines.
    pub bids: Vec<PlacementBid>,
    /// Roster machines that yielded no bid.
    pub silent_machines: BTreeSet<MachineRowId>,
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

/// One bidder dropped at tier zero, with the reason it can never host this
/// deploy's containers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementElimination {
    pub machine_id: MachineRowId,
    pub reason: PlacementEliminationReason,
}

/// Why a bidder was dropped before any preference tier ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlacementEliminationReason {
    Draining,
    PlatformUnsupported { architecture: String },
    FreeDiskBelowFloor { free_disk_bytes: u64 },
    VolumeNotHeld { holder: MachineRowId },
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
        holders: Vec<MachineRowId>,
    },
    DarkVolumeHolder {
        machines: Vec<MachineRowId>,
    },
    VolumeReplicaLimit {
        requested: ServiceReplicaCount,
    },
}

/// Derives a deploy's target machine set from live bids.
///
/// Tier order: tier-0 drops (draining, wrong platform, free disk below
/// [`PLACEMENT_FREE_DISK_FLOOR_BYTES`], not the volume holder, outside the
/// pin set), then sticky (runs the incumbent's active deploy), then spread
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
        platforms,
        bids,
        silent_machines,
    } = inputs;

    let volume_pin = match placement {
        ServicePlacement::Replicated { replicas } => adjudicate_volumes(
            *replicas,
            volumes,
            plausible_volume_holders,
            bids,
            silent_machines,
        )?,
        ServicePlacement::Global { host_ports: _ } => None,
    };

    let mut eliminations = Vec::new();
    let mut survivors = Vec::new();
    for bid in bids {
        match tier_zero_drop(
            bid,
            pinned_machines,
            platforms.as_ref(),
            volume_pin.as_ref(),
        ) {
            Some(reason) => eliminations.push(PlacementElimination {
                machine_id: bid.machine_id.clone(),
                reason,
            }),
            None => survivors.push(bid),
        }
    }
    if survivors.is_empty() {
        return Err(PlacementRefusal::NoEligibleMachines { eliminations });
    }

    let targets = match placement {
        ServicePlacement::Global { host_ports: _ } => {
            survivors.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
            return Ok(PlacementPick {
                targets: survivors
                    .into_iter()
                    .map(|bid| bid.machine_id.clone())
                    .collect(),
                eliminations,
                shortfall: None,
            });
        }
        ServicePlacement::Replicated { replicas } => {
            survivors.sort_by_key(|bid| preference_key(bid, active_deploy.as_ref()));
            round_robin_fill(&survivors, *replicas)
        }
    };
    let ServicePlacement::Replicated { replicas } = placement else {
        unreachable!("global placement returned above");
    };
    let shortfall = (usize::from(replicas.get()) > survivors.len()).then_some(PlacementShortfall {
        requested: *replicas,
        placed: survivors.len(),
    });
    Ok(PlacementPick {
        targets,
        eliminations,
        shortfall,
    })
}

/// Resolves the volume affinity rules for a replicated service. Returns the
/// single visible holder the pick must pin to, or `None` when the pick runs
/// normally (no volumes, or a first deploy with no holder anywhere).
fn adjudicate_volumes(
    replicas: ServiceReplicaCount,
    volumes: &BTreeSet<VolumeName>,
    plausible_volume_holders: &BTreeSet<MachineRowId>,
    bids: &[PlacementBid],
    silent_machines: &BTreeSet<MachineRowId>,
) -> Result<Option<MachineRowId>, PlacementRefusal> {
    if volumes.is_empty() {
        return Ok(None);
    }
    if replicas.get() > 1 {
        return Err(PlacementRefusal::VolumeReplicaLimit {
            requested: replicas,
        });
    }
    // A silent plausible holder wins over every visible holder: the dark
    // machine may hold a fork of the data, and a fork deserves a human.
    let dark_holders = plausible_volume_holders
        .intersection(silent_machines)
        .cloned()
        .collect::<Vec<_>>();
    if !dark_holders.is_empty() {
        return Err(PlacementRefusal::DarkVolumeHolder {
            machines: dark_holders,
        });
    }
    let mut holders_by_volume = BTreeMap::new();
    for bid in bids {
        for volume in bid.volumes_held.intersection(volumes) {
            holders_by_volume
                .entry(volume.clone())
                .or_insert_with(BTreeSet::new)
                .insert(bid.machine_id.clone());
        }
    }
    let distinct_holders = holders_by_volume
        .values()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    if distinct_holders.len() > 1 {
        let (volume, _) = holders_by_volume
            .iter()
            .find(|(_, holders)| !holders.is_empty())
            .expect("multiple distinct holders imply a held volume");
        return Err(PlacementRefusal::VolumeHolderConflict {
            volume: volume.clone(),
            holders: distinct_holders.into_iter().collect(),
        });
    }
    Ok(distinct_holders.into_iter().next())
}

fn tier_zero_drop(
    bid: &PlacementBid,
    pinned_machines: &BTreeSet<MachineRowId>,
    platforms: Option<&BTreeSet<OciPlatform>>,
    volume_pin: Option<&MachineRowId>,
) -> Option<PlacementEliminationReason> {
    match bid.lifecycle {
        MachineLifecycle::Draining => return Some(PlacementEliminationReason::Draining),
        MachineLifecycle::Active => {}
    }
    if let Some(platforms) = platforms
        && !supports_platform(platforms, &bid.architecture)
    {
        return Some(PlacementEliminationReason::PlatformUnsupported {
            architecture: bid.architecture.clone(),
        });
    }
    if bid.free_disk_bytes < PLACEMENT_FREE_DISK_FLOOR_BYTES {
        return Some(PlacementEliminationReason::FreeDiskBelowFloor {
            free_disk_bytes: bid.free_disk_bytes,
        });
    }
    if let Some(holder) = volume_pin
        && &bid.machine_id != holder
    {
        return Some(PlacementEliminationReason::VolumeNotHeld {
            holder: holder.clone(),
        });
    }
    if !pinned_machines.is_empty() && !pinned_machines.contains(&bid.machine_id) {
        return Some(PlacementEliminationReason::OutsidePinSet);
    }
    None
}

/// A machine supports the image when any declared platform matches its
/// normalized architecture. A bid architecture that is not a valid OCI
/// component can never match a declared platform.
fn supports_platform(platforms: &BTreeSet<OciPlatform>, architecture: &str) -> bool {
    let Ok(machine) = OciPlatform::try_new("linux", architecture) else {
        return false;
    };
    platforms
        .iter()
        .any(|platform| platform.architecture() == machine.architecture())
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
