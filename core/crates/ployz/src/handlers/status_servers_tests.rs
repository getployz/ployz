use std::net::Ipv4Addr;

use std::num::NonZeroU32;

use ployz_core::{
    ContainerAddress, ContainerKind, ContainerObservation, ContainerObservationParts,
    DeploymentLogId, HealthObservation, Machine, MachineFailure, MachineSuccess, PartialResult,
    RpcErrorCode, ServiceMode, ServiceName, service_containers,
};
use serde_json::json;

use super::*;
use crate::global_fixtures::{container_id, machine, qualified, running_on, service_id};

fn machines() -> [Machine; 3] {
    [
        machine('a', "machine-1"),
        machine('b', "machine-2"),
        machine('c', "machine-3"),
    ]
}

fn rotated_on(machine: &Machine, hex: char, rotation: CopyRotation) -> ContainerObservation {
    let mut copy = running_on(machine, hex);
    copy.try_update(|parts| {
        parts.rotation = Some(rotation);
        parts.address = Some(ContainerAddress(Ipv4Addr::new(10, 210, 0, 1)));
    })
    .unwrap();
    copy
}

fn routed_on(machine: &Machine, hex: char) -> ContainerObservation {
    rotated_on(machine, hex, CopyRotation::In)
}

fn held() -> CopyRotation {
    CopyRotation::Held(DeploymentLogId::parse("run").unwrap())
}

fn service(name: &str, id: char, copies: [ContainerObservation; 2]) -> ServiceObservation {
    ServiceObservation {
        identity: qualified("app", name),
        service_id: service_id(id),
        containers: service_containers(copies),
        hook_containers: Vec::new(),
    }
}

fn web(copies: [ContainerObservation; 2]) -> ServiceObservation {
    service("web", 'a', copies)
}

fn routes_as_ployzd_answers(acked: &[(&Machine, &[char])], down: &[&Machine]) -> RoutesRead {
    RoutesRead {
        routes: acked
            .iter()
            .map(|(machine, copies)| {
                let copies = copies.iter().copied().map(container_id).collect();
                (machine.id, Some(copies))
            })
            .collect(),
        complete: true,
        down: down.iter().map(|machine| machine.id).collect(),
    }
}

fn listed(up: &[&Machine], down: &[&Machine]) -> Vec<MachineObservation> {
    let up = up
        .iter()
        .map(|machine| (machine, MembershipObservation::Up));
    let down = down
        .iter()
        .map(|machine| (machine, MembershipObservation::Down));
    up.chain(down)
        .map(|(machine, membership)| MachineObservation::new((*machine).clone(), membership))
        .collect()
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.to_owned(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

fn listing(failed: &[&Machine]) -> LiveServices<RpcError> {
    LiveServices {
        containers: PartialResult {
            successes: Vec::new(),
            failures: failed
                .iter()
                .map(|machine| MachineFailure {
                    machine_id: machine.id,
                    error: unavailable("connection refused"),
                })
                .collect(),
            omissions: Vec::new(),
        },
    }
}

fn checked(
    machines: Vec<MachineObservation>,
    reads: Vec<(ServiceObservation, Result<RoutesRead, RpcError>)>,
) -> Check {
    checked_routing(machines, reads, BTreeMap::new())
}

fn checked_routing(
    machines: Vec<MachineObservation>,
    reads: Vec<(ServiceObservation, Result<RoutesRead, RpcError>)>,
    routed: BTreeMap<MachineId, RoutedAddresses>,
) -> Check {
    judged_in_app(Observed {
        machines,
        copies: Copies::Listed {
            live: listing(&[]),
            reads,
            routed: routed
                .into_iter()
                .map(|(machine, routed)| (machine, Ok(routed)))
                .collect(),
        },
    })
}

fn web_routed_at(copies: &[(char, [u8; 4])]) -> RoutedAddresses {
    RoutedAddresses {
        routes: Some(BTreeMap::from([(
            qualified("app", "web"),
            copies
                .iter()
                .map(|(hex, address)| {
                    (
                        container_id(*hex),
                        ContainerAddress(Ipv4Addr::from(*address)),
                    )
                })
                .collect(),
        )])),
    }
}

/// Judge what status observed for namespace `app`, with hints that name no scope.
fn judged_in_app(observed: Observed) -> Check {
    judge(observed, &qualified("app", "web").namespace, |words| {
        format!("ployz {}", words.join(" "))
    })
}

fn warned(check: &Check) -> Vec<String> {
    check.warn();
    let (_, json) = crate::ui::captured(|| crate::ui::show(&json!({})));
    json.and_then(|json| json.get("warnings").cloned())
        .map(|warnings| serde_json::from_value(warnings).unwrap())
        .unwrap_or_default()
}

/// A copy of `app/web` as its owner lists it, with the Deploy's `replicas` in its spec.
fn web_copy(
    machine: &Machine,
    hex: char,
    rotation: CopyRotation,
    runtime: ContainerRuntimeObservation,
    replicas: u32,
    created: i64,
) -> ContainerObservation {
    let mut copy = rotated_on(machine, hex, rotation);
    copy.try_update(|parts| {
        parts.runtime = runtime;
        parts.created_at_unix_nanos = created;
        parts.resolved_spec.name = ServiceName::parse("web").unwrap();
        parts.resolved_spec.mode = ServiceMode::Replicated {
            replicas: NonZeroU32::new(replicas).unwrap(),
        };
    })
    .unwrap();
    copy
}

fn listed_copies(copies: Vec<ContainerObservation>) -> LiveServices<RpcError> {
    let mut by_machine: Vec<MachineSuccess<Vec<ContainerObservation>>> = Vec::new();
    for copy in copies {
        match by_machine
            .iter_mut()
            .find(|success| success.machine_id == copy.machine_id)
        {
            Some(success) => success.value.push(copy),
            None => by_machine.push(MachineSuccess {
                machine_id: copy.machine_id,
                value: vec![copy],
            }),
        }
    }
    LiveServices {
        containers: PartialResult {
            successes: by_machine,
            failures: Vec::new(),
            omissions: Vec::new(),
        },
    }
}

/// Judge the listing alone: no Service has a Serving Container whose routes were read.
fn judged(machines: Vec<MachineObservation>, live: LiveServices<RpcError>) -> Check {
    judged_in_app(Observed {
        machines,
        copies: Copies::Listed {
            live,
            reads: Vec::new(),
            routed: BTreeMap::new(),
        },
    })
}

#[test]
fn status_names_a_service_whose_copies_crash() {
    let [one, two, three] = machines();
    let check = judged(
        listed(&[&one, &two, &three], &[]),
        listed_copies(vec![
            web_copy(
                &one,
                '1',
                CopyRotation::In,
                ContainerRuntimeObservation::Restarting,
                2,
                1,
            ),
            web_copy(
                &two,
                '2',
                CopyRotation::In,
                ContainerRuntimeObservation::Exited { code: 1 },
                2,
                1,
            ),
        ]),
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "copies_not_serving",
            "message": "Serving 0 of 2 copies. machine-1 restarting, machine-2 exited with code 1.",
            "next": "ployz logs web",
            "service": "app/web",
            "serving": 0,
            "wanted": 2,
            "copies": [
                {"machine": "machine-1", "container": "1".repeat(64), "rotation": "in", "runtime": {"state": "restarting"}},
                {"machine": "machine-2", "container": "2".repeat(64), "rotation": "in", "runtime": {"state": "exited", "code": 1}},
            ],
        }])
    );
    assert!(warned(&check).is_empty());
    assert_eq!(
        super::super::link::rendered(check.attention),
        "organization  ?\nsigned in as  local\nweb           Serving 0 of 2 copies. machine-1 restarting, machine-2 exited with code 1.\n"
    );
}

#[test]
fn status_names_a_service_left_draining() {
    let [one, two, three] = machines();
    let running = || ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    };
    let check = judged(
        listed(&[&one, &two, &three], &[]),
        listed_copies(vec![
            web_copy(&one, '1', CopyRotation::Out, running(), 3, 1),
            web_copy(&two, '2', CopyRotation::Out, running(), 3, 1),
            web_copy(&three, '3', CopyRotation::In, running(), 3, 2),
        ]),
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "copies_not_serving",
            "message": "Serving 1 of 3 copies. machine-1 draining, machine-2 draining.",
            "next": "ployz deploy web",
            "service": "app/web",
            "serving": 1,
            "wanted": 3,
            "copies": [
                {"machine": "machine-1", "container": "1".repeat(64), "rotation": "out", "runtime": {"state": "running", "health": "healthy"}},
                {"machine": "machine-2", "container": "2".repeat(64), "rotation": "out", "runtime": {"state": "running", "health": "healthy"}},
            ],
        }])
    );
    assert!(warned(&check).is_empty());
    assert_eq!(
        super::super::link::rendered(check.attention),
        "organization  ?\nsigned in as  local\nweb           Serving 1 of 3 copies. machine-1 draining, machine-2 draining.\n"
    );
}

fn healthy() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    }
}

fn edited(
    mut copy: ContainerObservation,
    edit: impl FnOnce(&mut ContainerObservationParts),
) -> ContainerObservation {
    copy.try_update(edit).unwrap();
    copy
}

fn judged_copies(copies: Vec<ContainerObservation>) -> Check {
    let [one, two, three] = machines();
    judged(listed(&[&one, &two, &three], &[]), listed_copies(copies))
}

fn messages(check: &Check) -> Vec<&str> {
    check
        .attention
        .iter()
        .map(|attention| attention.message.as_str())
        .collect()
}

#[test]
fn no_row_while_the_old_copies_of_a_finished_deploy_still_drain() {
    let [one, two, _] = machines();
    let check = judged_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 2, 2),
        web_copy(&two, '2', CopyRotation::In, healthy(), 2, 2),
        web_copy(&one, '3', CopyRotation::Out, healthy(), 2, 1),
        web_copy(&two, '4', CopyRotation::Out, healthy(), 2, 1),
    ]);

    assert!(check.attention.is_empty());
    assert!(warned(&check).is_empty());
}

#[test]
fn while_a_deploy_is_cancelling_status_skips_the_copy_check() {
    let [one, two, three] = machines();
    let check = judged_in_app(Observed {
        machines: listed(&[&one, &two, &three], &[]),
        copies: Copies::RollingOut(rolling_out(&[deployment(12, "cancelling")]).unwrap()),
    });

    assert!(check.attention.is_empty());
    assert_eq!(
        warned(&check),
        [
            "Not checking which copies serve or which Servers picked up the new copies while deployment #12 is in progress."
        ]
    );
}

#[test]
fn a_surplus_out_copy_beside_a_full_set_is_no_row() {
    let [one, two, _] = machines();
    let check = judged_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 1, 2),
        web_copy(&two, '2', CopyRotation::Out, healthy(), 1, 1),
    ]);

    assert!(check.attention.is_empty());
}

#[test]
fn a_count_left_high_by_a_scale_down_is_no_row() {
    let [one, ..] = machines();
    let check = judged_copies(vec![web_copy(&one, '1', CopyRotation::In, healthy(), 3, 1)]);

    assert!(check.attention.is_empty());
}

#[test]
fn a_copy_the_user_stopped_is_no_row() {
    let [one, ..] = machines();
    let stopped = edited(
        web_copy(
            &one,
            '1',
            held(),
            ContainerRuntimeObservation::Exited { code: 0 },
            1,
            1,
        ),
        |parts| parts.stopped = true,
    );

    assert!(judged_copies(vec![stopped]).attention.is_empty());
}

#[test]
fn an_out_copy_that_exited_is_no_row() {
    let [one, ..] = machines();
    let check = judged_copies(vec![web_copy(
        &one,
        '1',
        CopyRotation::Out,
        ContainerRuntimeObservation::Exited { code: 1 },
        1,
        1,
    )]);

    assert!(check.attention.is_empty());
}

#[test]
fn a_pending_copy_that_restarts_is_no_row() {
    let [one, ..] = machines();
    let check = judged_copies(vec![web_copy(
        &one,
        '1',
        CopyRotation::Pending,
        ContainerRuntimeObservation::Restarting,
        1,
        1,
    )]);

    assert!(check.attention.is_empty());
}

fn cut_for_a_volume_move(machine: &Machine) -> ContainerObservation {
    web_copy(
        machine,
        '1',
        CopyRotation::In,
        ContainerRuntimeObservation::Exited { code: 143 },
        1,
        1,
    )
}

#[test]
fn status_names_the_copy_a_volume_move_cut_until_its_new_copy_is_in() {
    let [one, ..] = machines();
    let check = judged_copies(vec![cut_for_a_volume_move(&one)]);

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "copies_not_serving",
            "message": "Serving 0 of 1 copy. machine-1 exited with code 143.",
            "next": "ployz logs web",
            "service": "app/web",
            "serving": 0,
            "wanted": 1,
            "copies": [
                {"machine": "machine-1", "container": "1".repeat(64), "rotation": "in", "runtime": {"state": "exited", "code": 143}},
            ],
        }])
    );
}

#[test]
fn a_volume_move_whose_new_copy_is_in_is_no_row() {
    let [one, two, _] = machines();
    let check = judged_copies(vec![
        cut_for_a_volume_move(&one),
        web_copy(&two, '2', CopyRotation::In, healthy(), 1, 2),
    ]);

    assert!(check.attention.is_empty());
}

#[test]
fn a_crashing_copy_on_a_down_machine_shows_only_the_down_row() {
    let [one, two, three] = machines();
    let check = judged(
        listed(&[&one, &two], &[&three]),
        listed_copies(vec![web_copy(
            &three,
            '3',
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            1,
            1,
        )]),
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "machine-3",
        }])
    );
}

#[test]
fn status_names_a_global_service_with_a_copy_that_restarts() {
    let [one, two, _] = machines();
    let check = judged_copies(vec![
        global(web_copy(&one, '1', CopyRotation::In, healthy(), 1, 1)),
        global(web_copy(
            &two,
            '2',
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            1,
            1,
        )),
    ]);

    assert_eq!(
        messages(&check),
        ["Serving 1 of 2 copies. machine-2 restarting."]
    );
}

fn global(copy: ContainerObservation) -> ContainerObservation {
    edited(copy, |parts| parts.resolved_spec.mode = ServiceMode::Global)
}

#[test]
fn a_global_copy_left_draining_on_a_drained_server_is_no_row() {
    let [one, two, three] = machines();
    let check = judged_copies(vec![
        global(web_copy(&one, '1', CopyRotation::In, healthy(), 1, 2)),
        global(web_copy(&two, '2', CopyRotation::In, healthy(), 1, 2)),
        global(web_copy(&three, '3', CopyRotation::Out, healthy(), 1, 1)),
    ]);

    assert!(check.attention.is_empty());
}

#[test]
fn a_global_copy_replaced_on_its_server_is_no_row() {
    let [one, ..] = machines();
    let check = judged_copies(vec![
        global(web_copy(&one, '1', CopyRotation::Out, healthy(), 1, 1)),
        global(web_copy(&one, '2', CopyRotation::In, healthy(), 1, 2)),
    ]);

    assert!(check.attention.is_empty());
}

#[test]
fn a_copy_held_through_a_restart_beside_a_surplus_out_copy_is_no_row() {
    let [one, two, three] = machines();
    let check = judged_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 2, 2),
        web_copy(&two, '2', held(), healthy(), 2, 2),
        web_copy(&three, '3', CopyRotation::Out, healthy(), 2, 1),
    ]);

    assert!(check.attention.is_empty());
}

#[test]
fn a_leftover_out_copy_after_a_scale_down_is_named_until_a_deploy_removes_it() {
    let [one, two, three] = machines();
    let check = judged_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 3, 2),
        web_copy(&two, '2', CopyRotation::In, healthy(), 3, 2),
        web_copy(&three, '3', CopyRotation::Out, healthy(), 3, 1),
    ]);

    assert_eq!(
        messages(&check),
        ["Serving 2 of 3 copies. machine-3 draining."]
    );
}

#[test]
fn the_count_comes_from_the_newest_kept_copy_not_from_a_failed_deploy() {
    let [one, two, three] = machines();
    let restarting = || ContainerRuntimeObservation::Restarting;
    let check = judged_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 4, 1),
        web_copy(&two, '2', CopyRotation::In, healthy(), 4, 1),
        web_copy(&three, '3', CopyRotation::In, restarting(), 4, 1),
        web_copy(&one, '4', CopyRotation::In, restarting(), 4, 1),
        web_copy(&two, '5', CopyRotation::Pending, healthy(), 2, 2),
        web_copy(&three, '6', CopyRotation::Pending, healthy(), 2, 2),
    ]);

    assert_eq!(
        messages(&check),
        ["Serving 2 of 4 copies. machine-1 restarting, machine-3 restarting."]
    );
}

#[test]
fn a_hook_container_counts_neither_as_serving_nor_as_kept() {
    let [one, two, three] = machines();
    let hook = |copy| edited(copy, |parts| parts.kind = ContainerKind::PreDeployHook);
    let check = judged_copies(vec![
        web_copy(
            &one,
            '1',
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            3,
            1,
        ),
        hook(web_copy(&two, '2', CopyRotation::In, healthy(), 3, 2)),
        hook(web_copy(
            &three,
            '3',
            CopyRotation::In,
            ContainerRuntimeObservation::Exited { code: 1 },
            3,
            2,
        )),
    ]);

    assert_eq!(
        messages(&check),
        ["Serving 0 of 1 copy. machine-1 restarting."]
    );
}

fn unanswered_by(mut live: LiveServices<RpcError>, machine: &Machine) -> LiveServices<RpcError> {
    live.containers.failures.push(MachineFailure {
        machine_id: machine.id,
        error: unavailable("connection refused"),
    });
    live
}

const MACHINE_2_DID_NOT_ANSWER: &str = "machine-2 did not answer, so status may miss copies that do not serve and Servers that have not picked up the new copies.";

#[test]
fn a_crashing_copy_on_a_server_that_did_not_answer_is_only_a_warning() {
    let [one, two, three] = machines();
    let check = judged(
        listed(&[&one, &two, &three], &[]),
        unanswered_by(
            listed_copies(vec![web_copy(&one, '1', CopyRotation::In, healthy(), 2, 1)]),
            &two,
        ),
    );

    assert!(check.attention.is_empty());
    assert_eq!(warned(&check), [MACHINE_2_DID_NOT_ANSWER]);
}

#[test]
fn an_out_copy_does_not_stand_in_for_a_copy_on_a_server_that_did_not_answer() {
    let [one, two, three] = machines();
    let check = judged(
        listed(&[&one, &two, &three], &[]),
        unanswered_by(
            listed_copies(vec![
                web_copy(&one, '1', CopyRotation::In, healthy(), 2, 2),
                web_copy(&three, '3', CopyRotation::Out, healthy(), 2, 1),
            ]),
            &two,
        ),
    );

    assert!(check.attention.is_empty());
    assert_eq!(warned(&check), [MACHINE_2_DID_NOT_ANSWER]);
}

#[test]
fn a_restarting_copy_is_named_against_the_copies_status_saw() {
    let [one, two, three] = machines();
    let check = judged(
        listed(&[&one, &two, &three], &[]),
        unanswered_by(
            listed_copies(vec![web_copy(
                &one,
                '1',
                CopyRotation::In,
                ContainerRuntimeObservation::Restarting,
                2,
                1,
            )]),
            &two,
        ),
    );

    assert_eq!(
        messages(&check),
        ["Serving 0 of 1 copy. machine-1 restarting."]
    );
    assert_eq!(warned(&check), [MACHINE_2_DID_NOT_ANSWER]);
}

#[test]
fn an_out_copy_beside_a_down_server_leaves_only_the_down_row() {
    let [one, two, three] = machines();
    let check = judged(
        listed(&[&one, &two], &[&three]),
        listed_copies(vec![
            web_copy(&one, '1', CopyRotation::In, healthy(), 3, 2),
            web_copy(&one, '4', CopyRotation::In, healthy(), 3, 2),
            web_copy(&two, '2', CopyRotation::Out, healthy(), 3, 1),
            web_copy(&three, '3', CopyRotation::In, healthy(), 3, 2),
        ]),
    );

    let reasons: Vec<&str> = check
        .attention
        .iter()
        .map(|attention| attention.reason.as_str())
        .collect();
    assert_eq!(reasons, ["machine_down"]);
}

#[test]
fn a_crashing_copy_in_another_namespace_is_no_row() {
    let [one, ..] = machines();
    let elsewhere = edited(
        web_copy(
            &one,
            '1',
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            1,
            1,
        ),
        |parts| parts.namespace = Namespace::parse("other").unwrap(),
    );

    assert!(judged_copies(vec![elsewhere]).attention.is_empty());
}

#[test]
fn a_copy_that_exited_cleanly_is_named_exited_not_crashed() {
    let [one, two, _] = machines();
    let check = judged_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 2, 1),
        web_copy(
            &two,
            '2',
            CopyRotation::In,
            ContainerRuntimeObservation::Exited { code: 0 },
            2,
            1,
        ),
    ]);

    assert_eq!(
        messages(&check),
        ["Serving 1 of 2 copies. machine-2 exited with code 0."]
    );
}

#[test]
fn status_names_an_in_copy_that_runs_unhealthy() {
    let [one, ..] = machines();
    let check = judged_copies(vec![web_copy(
        &one,
        '1',
        CopyRotation::In,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        },
        1,
        1,
    )]);

    assert_eq!(
        messages(&check),
        ["Serving 0 of 1 copy. machine-1 running, unhealthy."]
    );
}

#[test]
fn status_names_each_copy_on_one_machine() {
    let [one, ..] = machines();
    let check = judged_copies(vec![
        web_copy(
            &one,
            '1',
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            2,
            1,
        ),
        web_copy(
            &one,
            '2',
            CopyRotation::In,
            ContainerRuntimeObservation::Exited { code: 1 },
            2,
            1,
        ),
    ]);

    assert_eq!(
        messages(&check),
        ["Serving 0 of 2 copies. machine-1 restarting, machine-1 exited with code 1."]
    );
}

#[test]
fn status_names_every_copy_on_machines_that_share_a_name() {
    let [first, second] = [machine('a', "twin"), machine('b', "twin")];
    let restarting = |machine, hex| {
        web_copy(
            machine,
            hex,
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            2,
            1,
        )
    };
    let check = judged(
        listed(&[&first, &second], &[]),
        listed_copies(vec![restarting(&second, '1'), restarting(&first, '2')]),
    );

    let [
        Attention {
            message,
            subject: Some(Subject::NotServing { copies, .. }),
            ..
        },
    ] = check.attention.as_slice()
    else {
        panic!("one copies_not_serving row");
    };
    assert_eq!(
        message,
        "Serving 0 of 2 copies. twin restarting, twin restarting."
    );
    let containers: Vec<ContainerId> = copies.iter().map(|copy| copy.container).collect();
    assert_eq!(containers, [container_id('2'), container_id('1')]);
}

#[test]
fn status_points_to_the_logs_when_a_named_copy_is_in() {
    let [one, two, three] = machines();
    let check = judged_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 3, 2),
        web_copy(
            &two,
            '2',
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            3,
            2,
        ),
        web_copy(&three, '3', CopyRotation::Out, healthy(), 3, 1),
    ]);

    let hints: Vec<Option<&str>> = check
        .attention
        .iter()
        .map(|attention| attention.next.as_deref())
        .collect();
    assert_eq!(hints, [Some("ployz logs web")]);
}

#[test]
fn status_names_a_crashing_copy_before_a_machine_that_lacks_a_route() {
    let [one, two, three] = machines();
    let live = listed_copies(vec![
        web_copy(&one, '1', CopyRotation::In, healthy(), 2, 1),
        web_copy(
            &two,
            '2',
            CopyRotation::In,
            ContainerRuntimeObservation::Restarting,
            2,
            1,
        ),
    ]);
    let reads = live
        .services()
        .into_iter()
        .map(|service| {
            let read =
                routes_as_ployzd_answers(&[(&one, &['1']), (&two, &['1']), (&three, &['0'])], &[]);
            (service, Ok(read))
        })
        .collect();
    let check = judged_in_app(Observed {
        machines: listed(&[&one, &two, &three], &[]),
        copies: Copies::Listed {
            live,
            reads,
            routed: BTreeMap::new(),
        },
    });

    let reasons: Vec<&str> = check
        .attention
        .iter()
        .map(|attention| attention.reason.as_str())
        .collect();
    assert_eq!(reasons, ["copies_not_serving", "routes_not_acked"]);
}

#[test]
fn status_names_no_machine_when_every_proxy_acks_the_new_copies() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&two, '2')]),
            Ok(routes_as_ployzd_answers(
                &[
                    (&one, &['1', '2']),
                    (&two, &['1', '2']),
                    (&three, &['1', '2']),
                ],
                &[],
            )),
        )],
    );

    assert!(check.attention.is_empty());
    assert!(warned(&check).is_empty());
}

#[test]
fn status_names_a_live_machine_whose_proxy_lacks_a_new_copy() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&two, '2')]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1', '2']), (&two, &['1', '2']), (&three, &['0'])],
                &[],
            )),
        )],
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "routes_not_acked",
            "message": "machine-3 has not picked up the new copies. Check its network.",
            "service": "app/web",
            "machines": ["machine-3"],
        }])
    );
    assert_eq!(
        super::super::link::rendered(check.attention),
        "organization  ?\nsigned in as  local\nweb           machine-3 has not picked up the new copies. Check its network.\n"
    );
}

#[test]
fn status_names_a_machine_whose_proxy_routes_a_copy_at_an_address_its_owner_does_not_report() {
    let [one, two, three] = machines();
    let check = checked_routing(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&two, '2')]),
            Ok(routes_as_ployzd_answers(
                &[
                    (&one, &['1', '2']),
                    (&two, &['1', '2']),
                    (&three, &['1', '2']),
                ],
                &[],
            )),
        )],
        BTreeMap::from([
            (
                one.id,
                web_routed_at(&[('1', [10, 210, 0, 1]), ('2', [10, 210, 0, 1])]),
            ),
            (
                three.id,
                web_routed_at(&[('1', [10, 210, 2, 2]), ('2', [10, 210, 0, 1])]),
            ),
        ]),
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "routes_stale_address",
            "message": "machine-3 routes a copy to 10.210.2.2, which its owner does not report. Check its network.",
            "service": "app/web",
            "machine": "machine-3",
            "addresses": ["10.210.2.2"],
        }])
    );
    assert_eq!(
        super::super::link::rendered(check.attention),
        "organization  ?\nsigned in as  local\nweb           machine-3 routes a copy to 10.210.2.2, which its owner does not report. Check its network.\n"
    );
}

fn routing_a_removed_copy(
    listed_machines: Vec<MachineObservation>,
    unanswered: &[&Machine],
    removed_at: [u8; 4],
) -> Check {
    let [one, two, three] = machines();
    judged_in_app(Observed {
        machines: listed_machines,
        copies: Copies::Listed {
            live: listing(unanswered),
            reads: vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[
                        (&one, &['1', '3']),
                        (&two, &['1', '3']),
                        (&three, &['1', '3']),
                    ],
                    &[],
                )),
            )],
            routed: BTreeMap::from([(
                two.id,
                Ok(web_routed_at(&[('1', [10, 210, 0, 1]), ('0', removed_at)])),
            )]),
        },
    })
}

#[test]
fn status_names_a_machine_whose_proxy_routes_a_copy_its_owner_removed() {
    let [one, two, three] = machines();
    let check = routing_a_removed_copy(listed(&[&one, &two, &three], &[]), &[], [10, 210, 12, 5]);

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "routes_stale_address",
            "message": "machine-2 routes a copy to 10.210.12.5, which its owner does not report. Check its network.",
            "service": "app/web",
            "machine": "machine-2",
            "addresses": ["10.210.12.5"],
        }])
    );
}

#[test]
fn status_names_no_machine_for_a_removed_copy_whose_owner_did_not_answer() {
    let [one, two, three] = machines();
    let check = routing_a_removed_copy(
        listed(&[&one, &two, &three], &[]),
        &[&three],
        [10, 210, 12, 5],
    );

    assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
}

#[test]
fn status_names_no_machine_for_a_removed_copy_when_an_unanswered_machine_shares_its_subnet() {
    let [mut one, two, three] = machines();
    one.subnet = three.subnet;
    let check = routing_a_removed_copy(
        listed(&[&one, &two, &three], &[]),
        &[&three],
        [10, 210, 12, 5],
    );

    assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
}

#[test]
fn status_names_no_machine_for_a_removed_copy_when_a_down_machine_shares_its_subnet() {
    let [mut one, two, three] = machines();
    one.subnet = three.subnet;
    let check = routing_a_removed_copy(listed(&[&one, &two], &[&three]), &[], [10, 210, 12, 5]);

    assert!(
        check
            .attention
            .iter()
            .all(|attention| attention.reason == "machine_down"),
        "{}",
        serde_json::to_value(&check.attention).unwrap()
    );
}

#[test]
fn status_names_no_machine_for_a_removed_copy_whose_owner_is_down() {
    let [one, two, three] = machines();
    let check = routing_a_removed_copy(listed(&[&one, &two], &[&three]), &[], [10, 210, 12, 5]);

    assert!(
        check
            .attention
            .iter()
            .all(|attention| attention.reason == "machine_down"),
        "{}",
        serde_json::to_value(&check.attention).unwrap()
    );
}

#[test]
fn status_names_no_machine_for_a_removed_copy_whose_owner_the_reader_calls_down() {
    let [one, two, three] = machines();
    let check = judged_in_app(Observed {
        machines: listed(&[&one, &two, &three], &[]),
        copies: Copies::Listed {
            live: listing(&[]),
            reads: vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1', '3']), (&two, &['1', '3'])],
                    &[&three],
                )),
            )],
            routed: BTreeMap::from([(
                two.id,
                Ok(web_routed_at(&[
                    ('1', [10, 210, 0, 1]),
                    ('0', [10, 210, 12, 5]),
                ])),
            )]),
        },
    });

    assert!(
        check
            .attention
            .iter()
            .all(|attention| attention.reason != "routes_stale_address"),
        "{}",
        serde_json::to_value(&check.attention).unwrap()
    );
}

#[test]
fn status_names_no_machine_whose_listing_failed_for_what_its_proxy_routes() {
    let [one, two, three] = machines();
    let check = routing_a_removed_copy(
        listed(&[&one, &two, &three], &[]),
        &[&two],
        [10, 210, 12, 5],
    );

    assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
    assert_eq!(
        warned(&check),
        [
            "machine-2 did not answer, so status may miss copies that do not serve and Servers that have not picked up the new copies."
        ]
    );
}

#[test]
fn status_names_no_machine_for_a_removed_copy_at_an_address_no_machine_owns() {
    let [one, two, three] = machines();
    let check = routing_a_removed_copy(listed(&[&one, &two, &three], &[]), &[], [10, 210, 99, 5]);

    assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
}

#[test]
fn status_warns_that_it_skips_a_machine_whose_proxy_config_is_unconfirmed() {
    let [one, two, three] = machines();
    let check = checked_routing(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&three, '3')]),
            Ok(routes_as_ployzd_answers(
                &[
                    (&one, &['1', '3']),
                    (&two, &['1', '3']),
                    (&three, &['1', '3']),
                ],
                &[],
            )),
        )],
        BTreeMap::from([
            (one.id, web_routed_at(&[('1', [10, 210, 0, 1])])),
            (two.id, RoutedAddresses { routes: None }),
        ]),
    );

    assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
    assert_eq!(
        warned(&check),
        [
            "machine-2 has not confirmed what its proxy serves, so status is not checking it for stale addresses."
        ]
    );
}

#[test]
fn status_warns_that_it_skips_a_machine_whose_routed_addresses_it_could_not_read() {
    let [one, two, three] = machines();
    let mut unsupported = unavailable("unknown method");
    unsupported.code = RpcErrorCode::Unsupported;
    let check = judged_in_app(Observed {
        machines: listed(&[&one, &two, &three], &[]),
        copies: Copies::Listed {
            live: listing(&[]),
            reads: vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[
                        (&one, &['1', '3']),
                        (&two, &['1', '3']),
                        (&three, &['1', '3']),
                    ],
                    &[],
                )),
            )],
            routed: BTreeMap::from([
                (one.id, Ok(web_routed_at(&[('1', [10, 210, 0, 1])]))),
                (two.id, Err(unavailable("deadline exceeded"))),
                (three.id, Err(unsupported)),
            ]),
        },
    });

    assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
    assert_eq!(
        warned(&check),
        [
            "machine-2 has not confirmed what its proxy serves, so status is not checking it for stale addresses."
        ],
        "an older Server that lacks the check stays silent"
    );
}

fn checked_with_an_old_copy(rotation: CopyRotation) -> Check {
    let [one, two, three] = machines();
    checked(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([routed_on(&one, '1'), rotated_on(&two, '0', rotation)]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&two, &['1']), (&three, &['1', '0'])],
                &[],
            )),
        )],
    )
}

#[test]
fn status_names_a_live_machine_whose_proxy_still_routes_an_out_copy() {
    let check = checked_with_an_old_copy(CopyRotation::Out);

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "routes_not_acked",
            "message": "machine-3 has not picked up the new copies. Check its network.",
            "service": "app/web",
            "machines": ["machine-3"],
        }])
    );
    assert!(warned(&check).is_empty());
}

#[test]
fn status_names_a_live_machine_whose_proxy_still_routes_a_held_copy() {
    let check = checked_with_an_old_copy(held());

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "routes_not_acked",
            "message": "machine-3 has not picked up the new copies. Check its network.",
            "service": "app/web",
            "machines": ["machine-3"],
        }])
    );
    assert!(warned(&check).is_empty());
}

#[test]
fn status_names_once_each_machine_that_lacks_a_new_copy_or_routes_an_old_one() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([
                routed_on(&one, '1'),
                rotated_on(&two, '0', CopyRotation::Out),
            ]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&two, &['0']), (&three, &['1', '0'])],
                &[],
            )),
        )],
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "routes_not_acked",
            "message": "machine-2, machine-3 have not picked up the new copies. Check their network.",
            "service": "app/web",
            "machines": ["machine-2", "machine-3"],
        }])
    );
}

#[test]
fn status_names_no_machine_for_routing_an_out_copy_on_a_down_machine() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &three], &[&two]),
        vec![(
            web([
                routed_on(&one, '1'),
                rotated_on(&two, '0', CopyRotation::Out),
            ]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&three, &['1', '0'])],
                &[],
            )),
        )],
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "machine-2",
        }])
    );
    assert!(warned(&check).is_empty());
}

#[test]
fn a_proxy_keeping_an_out_copy_while_every_in_copy_is_down_is_not_named() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &three], &[&two]),
        vec![(
            web([
                routed_on(&two, '1'),
                rotated_on(&one, '0', CopyRotation::Out),
            ]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['0']), (&three, &['0'])],
                &[],
            )),
        )],
    );

    assert_eq!(check.attention.len(), 1, "only machine-2's Down row");
    assert!(warned(&check).is_empty());
}

#[test]
fn status_names_no_machine_when_every_proxy_routes_only_the_new_copy() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([
                routed_on(&one, '1'),
                rotated_on(&two, '0', CopyRotation::Out),
            ]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&two, &['1']), (&three, &['1'])],
                &[],
            )),
        )],
    );

    assert!(check.attention.is_empty());
    assert!(warned(&check).is_empty());
}

#[test]
fn status_shows_a_machine_down_on_the_entry_and_the_reader_as_down_only() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two], &[&three]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&three, '3')]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&two, &['1'])],
                &[&three],
            )),
        )],
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "machine-3",
        }])
    );
    assert!(warned(&check).is_empty());
    assert_eq!(
        super::super::link::rendered(check.attention),
        "organization  ?\nsigned in as  local\nmachine-3     Down. Its copies serve again once it's back.\n"
    );
}

#[test]
fn status_shows_a_machine_down_only_on_the_entry_as_down_and_never_as_not_picking_up() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two], &[&three]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&three, '3')]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&two, &['1']), (&three, &[])],
                &[],
            )),
        )],
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "machine-3",
        }])
    );
    assert!(warned(&check).is_empty());
}

#[test]
fn status_warns_it_skips_a_machine_only_the_reader_calls_down() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&three, '3')]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&two, &['1'])],
                &[&three],
            )),
        )],
    );

    assert!(check.attention.is_empty(), "no routes_not_acked row");
    assert_eq!(
        warned(&check),
        [
            "The Server status asked sees machine-3 Down, so status is not checking what it routes or who routes its copies of web."
        ]
    );
}

#[test]
fn status_names_no_stale_address_for_a_machine_only_the_reader_calls_down() {
    let [one, two, three] = machines();
    let check = checked_routing(
        listed(&[&one, &two, &three], &[]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&three, '3')]),
            Ok(routes_as_ployzd_answers(
                &[(&one, &['1']), (&two, &['1'])],
                &[&three],
            )),
        )],
        BTreeMap::from([
            (
                two.id,
                web_routed_at(&[('1', [10, 210, 0, 1]), ('3', [10, 210, 2, 2])]),
            ),
            (
                three.id,
                web_routed_at(&[('1', [10, 210, 2, 2]), ('3', [10, 210, 0, 1])]),
            ),
        ]),
    );

    assert!(
        check.attention.is_empty(),
        "no routes_stale_address row for what machine-3 routes or for its copies"
    );
    assert_eq!(
        warned(&check),
        [
            "The Server status asked sees machine-3 Down, so status is not checking what it routes or who routes its copies of web."
        ]
    );
}

#[test]
fn a_service_whose_copies_are_all_on_down_machines_warns_nothing() {
    let [one, two, three] = machines();
    let mut read = routes_as_ployzd_answers(&[(&one, &[])], &[&two, &three]);
    read.complete = false;
    let check = checked(
        listed(&[&one, &two], &[&three]),
        vec![(
            web([routed_on(&three, '3'), routed_on(&three, '4')]),
            Ok(read),
        )],
    );

    assert_eq!(check.attention.len(), 1, "only machine-3's Down row");
    assert!(warned(&check).is_empty());
}

#[test]
fn status_shows_a_down_machine_when_no_service_has_copies() {
    let [one, two, three] = machines();

    let check = checked(listed(&[&one, &two], &[&three]), Vec::new());

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "machine-3",
        }])
    );
}

#[test]
fn status_says_the_check_is_partial_when_a_live_machine_did_not_list_its_copies() {
    let [one, two, three] = machines();
    let check = judged_in_app(Observed {
        machines: listed(&[&one, &two], &[&three]),
        copies: Copies::Listed {
            live: listing(&[&two, &three]),
            reads: Vec::new(),
            routed: BTreeMap::new(),
        },
    });

    assert_eq!(check.attention.len(), 1, "only machine-3's Down row");
    assert_eq!(
        warned(&check),
        [
            "machine-2 did not answer, so status may miss copies that do not serve and Servers that have not picked up the new copies."
        ]
    );
}

#[test]
fn a_failed_routes_read_warns_for_its_service_and_keeps_the_down_row() {
    let [one, two, three] = machines();
    let check = checked(
        listed(&[&one, &two], &[&three]),
        vec![(
            web([routed_on(&one, '1'), routed_on(&two, '2')]),
            Err(unavailable("store not ready")),
        )],
    );

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "machine-3",
        }])
    );
    assert_eq!(
        warned(&check),
        ["Could not check which Servers picked up the new copies of web: store not ready"]
    );
}

#[test]
fn a_read_that_lacks_the_full_machine_list_is_never_an_all_clear() {
    let [one, two, three] = machines();
    let mut read = routes_as_ployzd_answers(&[(&one, &['1', '2']), (&two, &['1', '2'])], &[]);
    read.complete = false;
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![(web([routed_on(&one, '1'), routed_on(&two, '2')]), Ok(read))],
    );

    assert!(check.attention.is_empty());
    assert_eq!(
        warned(&check),
        ["Could not check whether every Server picked up the new copies of web."]
    );
}

#[test]
fn a_machine_whose_routes_row_has_not_arrived_is_unreported_not_stale() {
    let [one, two, three] = machines();
    let mut read = routes_as_ployzd_answers(&[(&one, &['1', '2']), (&two, &['1', '2'])], &[]);
    read.routes.insert(three.id, None);
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![(web([routed_on(&one, '1'), routed_on(&two, '2')]), Ok(read))],
    );

    assert!(check.attention.is_empty(), "no routes_not_acked row");
    assert_eq!(
        warned(&check),
        ["machine-3 has not reported which copies it routes."]
    );
}

#[test]
fn a_read_that_lacks_the_full_machine_list_warns_once_for_every_service() {
    let [one, two, three] = machines();
    let mut read = routes_as_ployzd_answers(&[(&one, &['1', '2']), (&two, &['1', '2'])], &[]);
    read.complete = false;
    let copies = || [routed_on(&one, '1'), routed_on(&two, '2')];
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![
            (service("web", 'a', copies()), Ok(read.clone())),
            (service("api", 'b', copies()), Ok(read.clone())),
            (service("worker", 'c', copies()), Ok(read)),
        ],
    );

    assert_eq!(
        warned(&check),
        ["Could not check whether every Server picked up the new copies of web, api, worker."]
    );
}

#[test]
fn the_same_read_error_for_several_services_warns_once() {
    let [one, two, three] = machines();
    let copies = || [routed_on(&one, '1'), routed_on(&two, '2')];
    let check = checked(
        listed(&[&one, &two, &three], &[]),
        vec![
            (
                service("web", 'a', copies()),
                Err(unavailable("store not ready")),
            ),
            (
                service("api", 'b', copies()),
                Err(unavailable("store not ready")),
            ),
        ],
    );

    assert_eq!(
        warned(&check),
        ["Could not check which Servers picked up the new copies of web, api: store not ready"]
    );
}

fn deployment(number: u64, status: &str) -> DeploymentSummary {
    serde_json::from_value(json!({
        "id": "00000000-0000-4000-8000-000000000101",
        "environment_id": "00000000-0000-4000-8000-000000000002",
        "number": number, "status": status, "saved": 1, "services": [],
        "runner": null, "upload": null, "remove": false, "admitted_by": null,
        "admitted_at": 0, "started_at": null, "ended_at": null, "message": null,
        "push": null, "ci": null, "in_flight": true, "outcome": null,
    }))
    .unwrap()
}

#[test]
fn a_queued_deployment_does_not_turn_the_check_off() {
    assert_eq!(rolling_out(&[deployment(12, "queued")]), None);
}

#[test]
fn while_a_deployment_rolls_out_status_says_it_skips_the_check() {
    let [one, two, three] = machines();
    let deploying = [deployment(13, "queued"), deployment(12, "running")];
    let check = judged_in_app(Observed {
        machines: listed(&[&one, &two], &[&three]),
        copies: Copies::RollingOut(rolling_out(&deploying).unwrap()),
    });

    assert_eq!(
        serde_json::to_value(&check.attention).unwrap(),
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "machine-3",
        }])
    );
    assert_eq!(
        warned(&check),
        [
            "Not checking which copies serve or which Servers picked up the new copies while deployment #12 is in progress."
        ]
    );
}

fn status_warnings(config: &std::path::Path, flags: &[&str]) -> Vec<String> {
    let mut args = vec![
        "ployz",
        "status",
        "--ployz-config",
        config.to_str().unwrap(),
    ];
    args.extend(flags);
    let root = crate::cli::command().try_get_matches_from(args).unwrap();
    let namespace = qualified("app", "web").namespace;
    assert!(attention(&root, &namespace, &[]).is_empty());
    let (_, json) = crate::ui::captured(|| crate::ui::show(&json!({})));
    json.and_then(|json| json.get("warnings").cloned())
        .map(|warnings| serde_json::from_value(warnings).unwrap())
        .unwrap_or_default()
}

#[test]
fn status_warns_when_the_context_it_was_given_does_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    std::fs::write(&config, "contexts:\n  prod:\n    connections: []\n").unwrap();

    let warnings = status_warnings(&config, &["--context", "typo"]);

    let [warning] = warnings.as_slice() else {
        panic!("one warning: {warnings:?}");
    };
    assert!(
        warning.starts_with(&format!("{UNCHECKED}: context typo not found")),
        "{warning}"
    );
}

#[test]
fn status_checks_nothing_and_says_nothing_with_no_server_set_up() {
    if std::path::Path::new(crate::connect::DEFAULT_LOCAL_SOCKET).exists()
        || std::env::var_os(crate::cli::env::TOKEN).is_some()
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();

    assert!(status_warnings(&dir.path().join("config.yaml"), &[]).is_empty());
}
