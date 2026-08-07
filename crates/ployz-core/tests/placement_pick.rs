use std::collections::{BTreeMap, BTreeSet};

use ployz_core::corrosion::{
    HostPortBindings, MachineLoadBand, ServicePlacement, ServiceReplicaCount,
};
use ployz_core::deploy::VolumeName;
use ployz_core::ids::{ContainerId, MachineRowId, OperationRowId};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::placement::{
    PLACEMENT_FREE_DISK_FLOOR_BYTES, PlacementEliminationReason, PlacementMachine,
    PlacementPickInputs, PlacementRefusal, pick_placement,
};
use ployz_core::{PlacementBid, ServiceContainerObservation};

const MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
const MACHINE_C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAC";
const ACTIVE_DEPLOY: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAD";

fn machine(value: &str) -> MachineRowId {
    MachineRowId::try_new(value).expect("fixture machine id")
}

fn machine_name(machine_id: &str) -> MachineName {
    let name = match machine_id {
        MACHINE_A => "machine-a",
        MACHINE_B => "machine-b",
        MACHINE_C => "machine-c",
        other => panic!("no fixture name for machine {other}"),
    };
    MachineName::try_new(name).expect("fixture machine name")
}

fn placement_machine(machine_id: &str) -> PlacementMachine {
    PlacementMachine {
        machine_id: machine(machine_id),
        machine_name: machine_name(machine_id),
    }
}

fn operation(value: &str) -> OperationRowId {
    OperationRowId::try_new(value).expect("fixture operation id")
}

fn volume(value: &str) -> VolumeName {
    VolumeName::try_new(value).expect("fixture volume name")
}

fn replicas(value: u16) -> ServiceReplicaCount {
    ServiceReplicaCount::try_new(value).expect("fixture replica count")
}

fn silent(machine_ids: &[&str]) -> BTreeMap<MachineRowId, MachineName> {
    machine_ids
        .iter()
        .map(|machine_id| (machine(machine_id), machine_name(machine_id)))
        .collect()
}

fn bid(machine_id: &str) -> PlacementBid {
    PlacementBid {
        machine_id: machine(machine_id),
        machine_name: machine_name(machine_id),
        architecture: "x86_64".to_owned(),
        lifecycle: MachineLifecycle::Active,
        free_disk_bytes: 100 * PLACEMENT_FREE_DISK_FLOOR_BYTES,
        free_memory_bytes: 8 * 1024 * 1024 * 1024,
        load: MachineLoadBand::Normal,
        total_container_count: 0,
        service_containers: Vec::new(),
        volumes_held: BTreeSet::new(),
    }
}

fn service_container(deploy: &str) -> ServiceContainerObservation {
    ServiceContainerObservation {
        container_id: ContainerId::try_new("c0ffee").expect("fixture container id"),
        service_id: ployz_core::ids::ServiceRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAE")
            .expect("fixture service id"),
        deploy: operation(deploy),
        named_volumes: BTreeSet::new(),
    }
}

fn inputs(bids: Vec<PlacementBid>) -> PlacementPickInputs {
    PlacementPickInputs {
        placement: ServicePlacement::Replicated {
            replicas: replicas(1),
        },
        pinned_machines: BTreeSet::new(),
        volumes: BTreeSet::new(),
        plausible_volume_holders: BTreeSet::new(),
        active_deploy: None,
        bids,
        silent_machines: BTreeMap::new(),
    }
}

#[test]
fn a_draining_machine_is_dropped_at_tier_zero() {
    let mut draining = bid(MACHINE_A);
    draining.lifecycle = MachineLifecycle::Draining;
    let pick = pick_placement(&inputs(vec![draining, bid(MACHINE_B)])).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_B)]);
    let [elimination] = pick.eliminations.as_slice() else {
        panic!("exactly one elimination expected");
    };
    assert_eq!(elimination.machine_id, machine(MACHINE_A));
    assert_eq!(elimination.machine_name, machine_name(MACHINE_A));
    assert_eq!(elimination.reason, PlacementEliminationReason::Draining);
}

#[test]
fn a_machine_below_the_free_disk_floor_is_dropped_at_tier_zero() {
    let mut full = bid(MACHINE_A);
    full.free_disk_bytes = PLACEMENT_FREE_DISK_FLOOR_BYTES - 1;
    let pick = pick_placement(&inputs(vec![full, bid(MACHINE_B)])).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_B)]);
    let [elimination] = pick.eliminations.as_slice() else {
        panic!("exactly one elimination expected");
    };
    assert_eq!(
        elimination.reason,
        PlacementEliminationReason::FreeDiskBelowFloor {
            free_disk_bytes: PLACEMENT_FREE_DISK_FLOOR_BYTES - 1,
        }
    );
}

#[test]
fn machines_outside_the_pin_set_are_dropped_at_tier_zero() {
    let mut pinned = inputs(vec![bid(MACHINE_A), bid(MACHINE_B), bid(MACHINE_C)]);
    pinned.pinned_machines = BTreeSet::from([machine(MACHINE_B)]);
    let pick = pick_placement(&pinned).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_B)]);
    assert_eq!(pick.eliminations.len(), 2);
    for elimination in &pick.eliminations {
        assert_eq!(
            elimination.reason,
            PlacementEliminationReason::OutsidePinSet
        );
    }
}

#[test]
fn zero_eligible_bidders_is_the_only_capacity_refusal_and_names_every_drop() {
    let mut draining = bid(MACHINE_A);
    draining.lifecycle = MachineLifecycle::Draining;
    let mut full = bid(MACHINE_B);
    full.free_disk_bytes = 0;
    let refusal = pick_placement(&inputs(vec![draining, full])).expect_err("pick refuses");
    let PlacementRefusal::NoEligibleMachines { eliminations } = refusal else {
        panic!("zero survivors must refuse with the eliminations");
    };
    assert_eq!(eliminations.len(), 2);
}

#[test]
fn a_fully_dark_pin_set_refuses_with_no_eligible_machines() {
    let mut pinned = inputs(Vec::new());
    pinned.pinned_machines = BTreeSet::from([machine(MACHINE_A)]);
    pinned.silent_machines = silent(&[MACHINE_A]);
    let refusal = pick_placement(&pinned).expect_err("pick refuses");
    assert_eq!(
        refusal,
        PlacementRefusal::NoEligibleMachines {
            eliminations: Vec::new(),
        },
        "silent pins never bid, so nothing survives and nothing is eliminated"
    );
}

#[test]
fn sticky_beats_spread_so_the_incumbent_machine_keeps_its_service() {
    let mut incumbent_host = bid(MACHINE_C);
    incumbent_host.total_container_count = 5;
    incumbent_host.service_containers = vec![service_container(ACTIVE_DEPLOY)];
    let empty = bid(MACHINE_A);
    let mut sticky = inputs(vec![empty, incumbent_host]);
    sticky.active_deploy = Some(operation(ACTIVE_DEPLOY));
    let pick = pick_placement(&sticky).expect("pick succeeds");
    assert_eq!(
        pick.targets,
        vec![machine(MACHINE_C)],
        "a busier machine already running the active deploy wins over an empty one"
    );
}

#[test]
fn a_container_of_a_different_deploy_earns_no_stickiness() {
    let mut stale_host = bid(MACHINE_C);
    stale_host.total_container_count = 5;
    stale_host.service_containers = vec![service_container(MACHINE_B)];
    let mut sticky = inputs(vec![bid(MACHINE_A), stale_host]);
    sticky.active_deploy = Some(operation(ACTIVE_DEPLOY));
    let pick = pick_placement(&sticky).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_A)]);
}

#[test]
fn spread_prefers_the_machine_with_fewest_total_containers() {
    let mut busy = bid(MACHINE_A);
    busy.total_container_count = 4;
    let mut idle = bid(MACHINE_B);
    idle.total_container_count = 1;
    let pick = pick_placement(&inputs(vec![busy, idle])).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_B)]);
}

#[test]
fn load_band_breaks_spread_ties_idle_before_normal_before_hot() {
    let mut hot = bid(MACHINE_A);
    hot.load = MachineLoadBand::Hot;
    let mut idle = bid(MACHINE_B);
    idle.load = MachineLoadBand::Idle;
    let mut normal = bid(MACHINE_C);
    normal.load = MachineLoadBand::Normal;
    let mut three = inputs(vec![hot, idle, normal]);
    three.placement = ServicePlacement::Replicated {
        replicas: replicas(3),
    };
    let pick = pick_placement(&three).expect("pick succeeds");
    assert_eq!(
        pick.targets,
        vec![machine(MACHINE_B), machine(MACHINE_C), machine(MACHINE_A)]
    );
}

#[test]
fn the_lowest_machine_ulid_breaks_the_final_tie() {
    let pick =
        pick_placement(&inputs(vec![bid(MACHINE_B), bid(MACHINE_A)])).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_A)]);
}

#[test]
fn replicas_fill_round_robin_and_stack_with_a_loud_shortfall() {
    let mut stacked = inputs(vec![bid(MACHINE_A), bid(MACHINE_B)]);
    stacked.placement = ServicePlacement::Replicated {
        replicas: replicas(3),
    };
    let pick = pick_placement(&stacked).expect("pick succeeds");
    assert_eq!(
        pick.targets,
        vec![machine(MACHINE_A), machine(MACHINE_B), machine(MACHINE_A)],
        "the third replica stacks onto the first machine in round-robin order"
    );
    let shortfall = pick.shortfall.expect("stacking records a shortfall");
    assert_eq!(shortfall.requested, replicas(3));
    assert_eq!(shortfall.placed, 2);
}

#[test]
fn replicas_covered_by_distinct_machines_record_no_shortfall() {
    let mut spread = inputs(vec![bid(MACHINE_A), bid(MACHINE_B)]);
    spread.placement = ServicePlacement::Replicated {
        replicas: replicas(2),
    };
    let pick = pick_placement(&spread).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_A), machine(MACHINE_B)]);
    assert_eq!(pick.shortfall, None);
}

#[test]
fn the_pick_is_deterministic_for_identical_inputs() {
    let mut shuffled = inputs(vec![bid(MACHINE_C), bid(MACHINE_A), bid(MACHINE_B)]);
    shuffled.placement = ServicePlacement::Replicated {
        replicas: replicas(5),
    };
    let first = pick_placement(&shuffled).expect("pick succeeds");
    let second = pick_placement(&shuffled).expect("pick succeeds");
    assert_eq!(first, second);
}

#[test]
fn a_single_visible_volume_holder_pins_the_pick_to_it() {
    let mut holder = bid(MACHINE_B);
    holder.volumes_held = BTreeSet::from([volume("data")]);
    let mut with_volume = inputs(vec![bid(MACHINE_A), holder, bid(MACHINE_C)]);
    with_volume.volumes = BTreeSet::from([volume("data")]);
    let pick = pick_placement(&with_volume).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_B)]);
    assert_eq!(pick.eliminations.len(), 2);
    for elimination in &pick.eliminations {
        assert_eq!(
            elimination.reason,
            PlacementEliminationReason::VolumeNotHeld {
                holder: placement_machine(MACHINE_B),
            }
        );
    }
}

#[test]
fn zero_visible_holders_with_no_silent_plausible_holder_pick_normally() {
    let mut first_deploy = inputs(vec![bid(MACHINE_A), bid(MACHINE_B)]);
    first_deploy.volumes = BTreeSet::from([volume("data")]);
    let pick = pick_placement(&first_deploy).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_A)]);
}

#[test]
fn two_visible_holders_are_a_data_fork_and_refuse_with_both_named() {
    let mut left = bid(MACHINE_A);
    left.volumes_held = BTreeSet::from([volume("data")]);
    let mut right = bid(MACHINE_B);
    right.volumes_held = BTreeSet::from([volume("data")]);
    let mut forked = inputs(vec![left, right]);
    forked.volumes = BTreeSet::from([volume("data")]);
    let refusal = pick_placement(&forked).expect_err("pick refuses");
    assert_eq!(
        refusal,
        PlacementRefusal::VolumeHolderConflict {
            volume: volume("data"),
            holders: vec![placement_machine(MACHINE_A), placement_machine(MACHINE_B)],
        }
    );
}

#[test]
fn a_pin_on_one_of_two_visible_holders_resolves_the_fork_instead_of_refusing() {
    let mut left = bid(MACHINE_A);
    left.volumes_held = BTreeSet::from([volume("data")]);
    let mut right = bid(MACHINE_B);
    right.volumes_held = BTreeSet::from([volume("data")]);
    let mut pinned = inputs(vec![left, right]);
    pinned.volumes = BTreeSet::from([volume("data")]);
    pinned.pinned_machines = BTreeSet::from([machine(MACHINE_B)]);
    let pick = pick_placement(&pinned).expect("the pin adjudicates the fork");
    assert_eq!(pick.targets, vec![machine(MACHINE_B)]);
    let [elimination] = pick.eliminations.as_slice() else {
        panic!("exactly one elimination expected");
    };
    assert_eq!(elimination.machine_id, machine(MACHINE_A));
    assert_eq!(
        elimination.reason,
        PlacementEliminationReason::OutsidePinSet,
        "the unpinned holder is a pin drop, not false fork evidence"
    );
}

#[test]
fn a_multi_volume_split_refuses_as_no_eligible_machines_never_as_a_fork() {
    let mut left = bid(MACHINE_A);
    left.volumes_held = BTreeSet::from([volume("data")]);
    let mut right = bid(MACHINE_B);
    right.volumes_held = BTreeSet::from([volume("cache")]);
    let mut split = inputs(vec![left, right]);
    split.volumes = BTreeSet::from([volume("data"), volume("cache")]);
    let refusal = pick_placement(&split).expect_err("pick refuses");
    let PlacementRefusal::NoEligibleMachines { eliminations } = refusal else {
        panic!("a split across machines is a capacity refusal, never a fork");
    };
    assert_eq!(eliminations.len(), 2);
    for elimination in &eliminations {
        assert!(matches!(
            elimination.reason,
            PlacementEliminationReason::VolumeNotHeld { .. }
        ));
    }
}

#[test]
fn a_forked_volume_among_several_names_the_forked_volume_and_only_its_holders() {
    let mut left = bid(MACHINE_A);
    left.volumes_held = BTreeSet::from([volume("cache"), volume("data")]);
    let mut right = bid(MACHINE_B);
    right.volumes_held = BTreeSet::from([volume("data")]);
    let mut forked = inputs(vec![left, right, bid(MACHINE_C)]);
    forked.volumes = BTreeSet::from([volume("cache"), volume("data")]);
    let refusal = pick_placement(&forked).expect_err("pick refuses");
    assert_eq!(
        refusal,
        PlacementRefusal::VolumeHolderConflict {
            volume: volume("data"),
            holders: vec![placement_machine(MACHINE_A), placement_machine(MACHINE_B)],
        },
        "the single-holder volume never appears in the fork evidence"
    );
}

#[test]
fn a_silent_plausible_holder_refuses_even_when_a_visible_holder_answered() {
    let mut visible_holder = bid(MACHINE_A);
    visible_holder.volumes_held = BTreeSet::from([volume("data")]);
    let mut dark = inputs(vec![visible_holder]);
    dark.volumes = BTreeSet::from([volume("data")]);
    dark.plausible_volume_holders = BTreeSet::from([machine(MACHINE_A), machine(MACHINE_B)]);
    dark.silent_machines = silent(&[MACHINE_B]);
    let refusal = pick_placement(&dark).expect_err("pick refuses");
    assert_eq!(
        refusal,
        PlacementRefusal::DarkVolumeHolder {
            machines: vec![placement_machine(MACHINE_B)],
        },
        "the dark machine may hold a fork, which deserves a human over any visible holder"
    );
}

#[test]
fn a_dark_plausible_holder_refuses_even_when_a_pin_excludes_it() {
    let mut visible_holder = bid(MACHINE_A);
    visible_holder.volumes_held = BTreeSet::from([volume("data")]);
    let mut dark = inputs(vec![visible_holder]);
    dark.volumes = BTreeSet::from([volume("data")]);
    dark.pinned_machines = BTreeSet::from([machine(MACHINE_A)]);
    dark.plausible_volume_holders = BTreeSet::from([machine(MACHINE_A), machine(MACHINE_B)]);
    dark.silent_machines = silent(&[MACHINE_B]);
    let refusal = pick_placement(&dark).expect_err("pick refuses");
    assert_eq!(
        refusal,
        PlacementRefusal::DarkVolumeHolder {
            machines: vec![placement_machine(MACHINE_B)],
        },
        "the dark-holder check ignores pins: possible data loss outranks operator intent"
    );
}

#[test]
fn volume_services_refuse_more_than_one_replica() {
    let mut multi = inputs(vec![bid(MACHINE_A)]);
    multi.volumes = BTreeSet::from([volume("data")]);
    multi.placement = ServicePlacement::Replicated {
        replicas: replicas(2),
    };
    let refusal = pick_placement(&multi).expect_err("pick refuses");
    assert_eq!(
        refusal,
        PlacementRefusal::VolumeReplicaLimit {
            requested: replicas(2),
        }
    );
}

#[test]
fn a_global_service_targets_every_tier_zero_survivor_exactly_once() {
    let mut draining = bid(MACHINE_C);
    draining.lifecycle = MachineLifecycle::Draining;
    let mut global = inputs(vec![bid(MACHINE_B), bid(MACHINE_A), draining]);
    global.placement = ServicePlacement::Global {
        host_ports: HostPortBindings::default(),
    };
    let pick = pick_placement(&global).expect("pick succeeds");
    assert_eq!(pick.targets, vec![machine(MACHINE_A), machine(MACHINE_B)]);
    assert_eq!(pick.shortfall, None);
    assert_eq!(pick.eliminations.len(), 1);
}

#[test]
fn global_volume_services_run_no_holder_rules() {
    let mut holder = bid(MACHINE_A);
    holder.volumes_held = BTreeSet::from([volume("data")]);
    let mut also_holder = bid(MACHINE_B);
    also_holder.volumes_held = BTreeSet::from([volume("data")]);
    let mut global = inputs(vec![holder, also_holder]);
    global.placement = ServicePlacement::Global {
        host_ports: HostPortBindings::default(),
    };
    global.volumes = BTreeSet::from([volume("data")]);
    global.plausible_volume_holders = BTreeSet::from([machine(MACHINE_C)]);
    global.silent_machines = silent(&[MACHINE_C]);
    let pick = pick_placement(&global).expect("pick succeeds");
    assert_eq!(
        pick.targets,
        vec![machine(MACHINE_A), machine(MACHINE_B)],
        "each machine mounts its own local volume, so forks and dark holders never fire"
    );
}
