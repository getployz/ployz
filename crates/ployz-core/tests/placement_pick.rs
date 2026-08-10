use std::collections::BTreeSet;

use ployz_core::corrosion::{
    HostPortBindings, MachineLoadBand, ServicePlacement, ServiceReplicaCount,
};
use ployz_core::ids::{DeployName, MachineName};
use ployz_core::machine::MachineLifecycle;
use ployz_core::placement::{
    PLACEMENT_FREE_DISK_FLOOR_BYTES, PlacementBid, PlacementEliminationReason, PlacementPickInputs,
    PlacementRefusal, ServiceContainerObservation, pick_placement,
};

const MACHINE_A: &str = "machine-a";
const MACHINE_B: &str = "machine-b";
const MACHINE_C: &str = "machine-c";
const ACTIVE_DEPLOY: &str = "release-current";

fn machine(value: &str) -> MachineName {
    MachineName::try_new(value).expect("fixture machine name")
}

fn operation(value: &str) -> DeployName {
    DeployName::try_new(value).expect("fixture operation id")
}

fn replicas(value: u16) -> ServiceReplicaCount {
    ServiceReplicaCount::try_new(value).expect("fixture replica count")
}

fn bid(machine_name: &str) -> PlacementBid {
    PlacementBid {
        machine_name: machine(machine_name),
        lifecycle: MachineLifecycle::Active,
        endpoint_network_ready: true,
        free_disk_bytes: 100 * PLACEMENT_FREE_DISK_FLOOR_BYTES,
        load: MachineLoadBand::Normal,
        total_container_count: 0,
        service_containers: Vec::new(),
    }
}

fn service_container(deploy: &str) -> ServiceContainerObservation {
    ServiceContainerObservation {
        deploy: operation(deploy),
    }
}

fn inputs(bids: Vec<PlacementBid>) -> PlacementPickInputs {
    PlacementPickInputs {
        placement: ServicePlacement::Replicated {
            replicas: replicas(1),
        },
        pinned_machines: BTreeSet::new(),
        has_named_volumes: false,
        active_deploy: None,
        bids,
    }
}

#[test]
fn a_draining_machine_is_dropped_at_tier_zero() {
    let mut draining = bid(MACHINE_A);
    draining.lifecycle = MachineLifecycle::Draining;
    let targets = pick_placement(&inputs(vec![draining, bid(MACHINE_B)])).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_B)]);
}

#[test]
fn a_nonready_endpoint_network_is_dropped_only_for_new_placement() {
    let mut nonready = bid(MACHINE_A);
    nonready.endpoint_network_ready = false;
    let targets = pick_placement(&inputs(vec![nonready, bid(MACHINE_B)])).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_B)]);
}

#[test]
fn a_machine_below_the_free_disk_floor_is_dropped_at_tier_zero() {
    let mut full = bid(MACHINE_A);
    full.free_disk_bytes = PLACEMENT_FREE_DISK_FLOOR_BYTES - 1;
    let targets = pick_placement(&inputs(vec![full, bid(MACHINE_B)])).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_B)]);
}

#[test]
fn machines_outside_the_pin_set_are_dropped_at_tier_zero() {
    let mut pinned = inputs(vec![bid(MACHINE_A), bid(MACHINE_B), bid(MACHINE_C)]);
    pinned.pinned_machines = BTreeSet::from([machine(MACHINE_B)]);
    let targets = pick_placement(&pinned).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_B)]);
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
    let [draining, full] = eliminations.as_slice() else {
        panic!("both tier-zero drops must be retained in the refusal")
    };
    assert_eq!(draining.machine_name, machine(MACHINE_A));
    assert_eq!(draining.reason, PlacementEliminationReason::Draining);
    assert_eq!(full.machine_name, machine(MACHINE_B));
    assert_eq!(
        full.reason,
        PlacementEliminationReason::FreeDiskBelowFloor { free_disk_bytes: 0 }
    );
}

#[test]
fn a_fully_dark_pin_set_refuses_with_no_eligible_machines() {
    let mut pinned = inputs(Vec::new());
    pinned.pinned_machines = BTreeSet::from([machine(MACHINE_A)]);
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
    let targets = pick_placement(&sticky).expect("pick succeeds");
    assert_eq!(
        targets,
        vec![machine(MACHINE_C)],
        "a busier machine already running the active deploy wins over an empty one"
    );
}

#[test]
fn every_incumbent_replica_keeps_its_machine_even_when_ineligible() {
    let mut incumbent_host = bid(MACHINE_C);
    incumbent_host.lifecycle = MachineLifecycle::Draining;
    incumbent_host.service_containers = vec![
        service_container(ACTIVE_DEPLOY),
        service_container(ACTIVE_DEPLOY),
    ];
    let mut sticky = inputs(vec![bid(MACHINE_A), incumbent_host]);
    sticky.active_deploy = Some(operation(ACTIVE_DEPLOY));
    sticky.placement = ServicePlacement::Replicated {
        replicas: replicas(2),
    };

    let targets =
        pick_placement(&sticky).expect("incumbents do not need new-placement eligibility");
    assert_eq!(targets, vec![machine(MACHINE_C), machine(MACHINE_C)]);
}

#[test]
fn a_container_of_a_different_deploy_earns_no_stickiness() {
    let mut stale_host = bid(MACHINE_C);
    stale_host.total_container_count = 5;
    stale_host.service_containers = vec![service_container(MACHINE_B)];
    let mut sticky = inputs(vec![bid(MACHINE_A), stale_host]);
    sticky.active_deploy = Some(operation(ACTIVE_DEPLOY));
    let targets = pick_placement(&sticky).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_A)]);
}

#[test]
fn spread_prefers_the_machine_with_fewest_total_containers() {
    let mut busy = bid(MACHINE_A);
    busy.total_container_count = 4;
    let mut idle = bid(MACHINE_B);
    idle.total_container_count = 1;
    let targets = pick_placement(&inputs(vec![busy, idle])).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_B)]);
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
    let targets = pick_placement(&three).expect("pick succeeds");
    assert_eq!(
        targets,
        vec![machine(MACHINE_B), machine(MACHINE_C), machine(MACHINE_A)]
    );
}

#[test]
fn the_lowest_machine_name_breaks_the_final_tie() {
    let targets =
        pick_placement(&inputs(vec![bid(MACHINE_B), bid(MACHINE_A)])).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_A)]);
}

#[test]
fn replicas_fill_round_robin_and_stack() {
    let mut stacked = inputs(vec![bid(MACHINE_A), bid(MACHINE_B)]);
    stacked.placement = ServicePlacement::Replicated {
        replicas: replicas(3),
    };
    let targets = pick_placement(&stacked).expect("pick succeeds");
    assert_eq!(
        targets,
        vec![machine(MACHINE_A), machine(MACHINE_B), machine(MACHINE_A)],
        "the third replica stacks onto the first machine in round-robin order"
    );
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
fn a_single_replica_named_volume_first_deploy_picks_normally() {
    let mut first_deploy = inputs(vec![bid(MACHINE_A), bid(MACHINE_B)]);
    first_deploy.has_named_volumes = true;
    let targets = pick_placement(&first_deploy).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_A)]);
}

#[test]
fn volume_services_refuse_more_than_one_replica() {
    let mut multi = inputs(vec![bid(MACHINE_A)]);
    multi.has_named_volumes = true;
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
    let targets = pick_placement(&global).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_A), machine(MACHINE_B)]);
}

#[test]
fn global_named_volume_first_deploy_targets_every_survivor() {
    let mut global = inputs(vec![bid(MACHINE_A), bid(MACHINE_B)]);
    global.placement = ServicePlacement::Global {
        host_ports: HostPortBindings::default(),
    };
    global.has_named_volumes = true;
    let targets = pick_placement(&global).expect("pick succeeds");
    assert_eq!(targets, vec![machine(MACHINE_A), machine(MACHINE_B)]);
}
