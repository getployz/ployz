use std::time::{Duration, Instant};

use ployz_core::dataplane::{
    DataplaneProjection, DataplaneProjectionMember, DataplaneProjectionTestimony,
    EbpfAttachmentStatus, EndpointBridgeStatus, MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS,
    WireGuardDetectedMtu, WireGuardHandshakeStatus, WireGuardInterfaceMtu, WireGuardRttStatus,
};
use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployPlan, DeployPlanStep, DeployRequest, DeployServiceSpec,
    ImageReference, ImageSource, ReplicaCount,
};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, OperationEvent, OperationStatus,
};
use ployz_core::security::NatsPrincipal;
use ployz_core::state::ActiveMachineState;
use ployz_e2e::dind;
use ployz_sdk_types::{NetworkDataplaneTestimony, NetworkStatusMachine, NetworkStatusRequest};
use ployz_test_support::ids::{machine_id, namespace_id, service_id};
use ployzd::intent::service::NatsIntentReader;

use super::{
    CoreContext, DEPLOY_TERMINAL_BUDGET, DindMachine, NAMESPACE_ID_LABEL, WORKLOAD_IMAGE,
    add_and_join_edge, assert_unit_active, connect_core_client, finish, init_core_cluster,
    managed_workload_containers, reserved_deploy_request, terminal_operation_events,
    wait_for_machine_observations, wait_for_terminal_deploy_status, with_evidence,
};

/// Placement uses durable intent plus fresh machine testimony, fixes its peer
/// validation set for one pass, and does not require RTT.
#[tokio::test]
async fn scenario_intent_driven_placement_peer_health() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 2).await;
    with_evidence(&core.cluster, async {
        let [edge_2, edge_3] = core.cluster.edges() else {
            panic!("scenario requires exactly two edge machines");
        };
        for edge in [edge_2, edge_3] {
            add_and_join_edge(&core, edge).await;
        }
        for machine in ["core_1", "edge_2", "edge_3"] {
            wait_for_machine_observations(&core, &machine_id(machine)).await;
        }

        let controller = connect_core_client(
            &core,
            NatsPrincipal::Controller,
            &core.material.controller_seed,
        )
        .await
        .expect("connect controller for placement intent");
        let intent_reader = NatsIntentReader::new(controller);
        let intent = intent_reader.intent().await.expect("read placement intent");
        assert_declared_machines(&intent.dataplane_projection, &intent.active_machines);
        wait_for_ready_dataplane(&core, &intent.dataplane_projection).await;

        let stopped = core
            .exec_on(edge_3, &["systemctl", "stop", "ployzd-machine-edge_3"])
            .await;
        assert!(
            stopped.success(),
            "stop edge_3 machine role failed: {stopped:?}"
        );
        for machine in [core.cluster.core(), edge_2] {
            let dropped = core
                .exec_on(
                    machine,
                    &[
                        "iptables",
                        "-I",
                        "OUTPUT",
                        "1",
                        "-o",
                        "ployz-wg0",
                        "-p",
                        "icmp",
                        "-j",
                        "DROP",
                    ],
                )
                .await;
            assert!(
                dropped.success(),
                "suppress RTT on {} failed: {dropped:?}",
                machine.name
            );
        }

        let silent_intent = intent_reader
            .intent()
            .await
            .expect("read intent with silent edge");
        assert_declared_machines(
            &silent_intent.dataplane_projection,
            &silent_intent.active_machines,
        );
        wait_for_silent_edge_and_unavailable_rtt(&core, &silent_intent.dataplane_projection).await;

        let first_namespace = namespace_id("placement_silent");
        let first_plan = deploy_and_read_plan(
            &core,
            "idem_dind_placement_silent",
            placement_target("placement_silent"),
        )
        .await;
        assert_eq!(
            first_plan.target_machines(),
            [machine_id("core_1"), machine_id("edge_2")],
            "silent member or configured deploy_machines changed placement: {first_plan:?}"
        );
        assert_eq!(
            runtime_machines(&core, &first_namespace).await,
            [machine_id("core_1"), machine_id("edge_2")],
            "first deploy runtime did not match its plan"
        );

        let restarted = core
            .exec_on(edge_3, &["systemctl", "start", "ployzd-machine-edge_3"])
            .await;
        assert!(
            restarted.success(),
            "restart edge_3 machine role failed: {restarted:?}"
        );
        assert_unit_active(&core, edge_3, "ployzd-machine-edge_3").await;
        wait_for_machine_observations(&core, &machine_id("edge_3")).await;
        wait_for_ready_dataplane(&core, &intent.dataplane_projection).await;

        block_wireguard_pair(&core, core.cluster.core(), edge_2).await;
        block_wireguard_pair(&core, core.cluster.core(), edge_3).await;
        reset_wireguard_peer(
            &core,
            core.cluster.core(),
            projection_member(&intent.dataplane_projection, "edge_2"),
        )
        .await;
        reset_wireguard_peer(
            &core,
            edge_3,
            projection_member(&intent.dataplane_projection, "core_1"),
        )
        .await;

        let asymmetric = wait_for_asymmetric_handshakes(&core, &intent.dataplane_projection).await;
        assert!(
            asymmetric
                .iter()
                .all(|machine| locally_ready(machine, &intent.dataplane_projection)),
            "handshake fault changed local dataplane readiness: {asymmetric:?}"
        );

        let second_namespace = namespace_id("placement_single_pass");
        let second_plan = deploy_and_read_plan(
            &core,
            "idem_dind_placement_single_pass",
            placement_target("placement_single_pass"),
        )
        .await;
        let steps = second_plan
            .phases
            .iter()
            .flat_map(|phase| &phase.services)
            .flat_map(|service| &service.steps)
            .collect::<Vec<_>>();
        assert!(
            matches!(
                steps.as_slice(),
                [
                    DeployPlanStep::RunContainer { machine_id: first, .. },
                    DeployPlanStep::RunContainer { machine_id: second, .. },
                ] if first == &machine_id("edge_2") && second == &machine_id("edge_2")
            ),
            "fixed preliminary set did not leave only edge_2 eligible: {second_plan:?}"
        );
        assert_eq!(
            runtime_machines(&core, &second_namespace).await,
            [machine_id("edge_2"), machine_id("edge_2")],
            "single-pass deploy runtime did not match its plan"
        );
    })
    .await;

    finish(core).await;
}

fn assert_declared_machines(
    projection: &DataplaneProjection,
    active_machines: &[ActiveMachineState],
) {
    let expected = [
        machine_id("core_1"),
        machine_id("edge_2"),
        machine_id("edge_3"),
    ];
    assert_eq!(
        active_machines
            .iter()
            .map(|machine| machine.machine_id.clone())
            .collect::<Vec<_>>(),
        expected,
        "durable active-machine roster changed"
    );
    assert_eq!(
        projection
            .declared_members()
            .iter()
            .map(|member| member.machine_id.clone())
            .collect::<Vec<_>>(),
        expected,
        "declared dataplane projection changed"
    );
}

async fn network_status(core: &CoreContext) -> Vec<NetworkStatusMachine> {
    core.api
        .network_status(&NetworkStatusRequest::First {
            mode: ployz_sdk_types::NetworkStatusMode::Snapshot,
        })
        .await
        .expect("network status succeeds")
        .machines
}

async fn wait_for_ready_dataplane(core: &CoreContext, projection: &DataplaneProjection) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let machines = network_status(core).await;
        if machines.len() == 3
            && machines
                .iter()
                .all(|machine| locally_ready(machine, projection) && peer_handshakes_fresh(machine))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "three-machine dataplane did not become ready: {machines:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn locally_ready(machine: &NetworkStatusMachine, projection: &DataplaneProjection) -> bool {
    let Some(member) = projection
        .declared_members()
        .iter()
        .find(|member| member.machine_id == machine.active.machine_id)
    else {
        return false;
    };
    let NetworkDataplaneTestimony::Answered { value } = &machine.dataplane else {
        return false;
    };
    matches!(
        &value.projection.endpoint_bridge,
        EndpointBridgeStatus::Ready { subnet } if subnet == &member.endpoint_subnet
    ) && matches!(
        &value.projection.testimony,
        DataplaneProjectionTestimony::Applied { revisions }
            if revisions.declared_revision == *projection.declared_revision()
                && revisions.target_revision == *projection.target_revision()
    ) && matches!(value.ebpf_attachment, EbpfAttachmentStatus::Attached)
        && matches!(
            value.wireguard.detected_mtu,
            WireGuardDetectedMtu::Detected { .. }
        )
        && matches!(
            value.wireguard.interface_mtu,
            WireGuardInterfaceMtu::Detected { .. }
        )
}

fn peer_handshakes_fresh(machine: &NetworkStatusMachine) -> bool {
    let NetworkDataplaneTestimony::Answered { value } = &machine.dataplane else {
        return false;
    };
    value.wireguard.peers.iter().all(|peer| {
        matches!(
            peer.handshake,
            WireGuardHandshakeStatus::Ago { seconds }
                if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
        )
    })
}

async fn wait_for_silent_edge_and_unavailable_rtt(
    core: &CoreContext,
    projection: &DataplaneProjection,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let machines = network_status(core).await;
        let silent = machines.iter().any(|machine| {
            machine.active.machine_id == machine_id("edge_3")
                && matches!(machine.dataplane, NetworkDataplaneTestimony::NoAnswer)
        });
        let healthy_pair = ["core_1", "edge_2"].iter().all(|machine_id_value| {
            let Some(machine) = machines
                .iter()
                .find(|machine| machine.active.machine_id == machine_id(machine_id_value))
            else {
                return false;
            };
            locally_ready(machine, projection)
                && peer(
                    machine,
                    projection,
                    if *machine_id_value == "core_1" {
                        "edge_2"
                    } else {
                        "core_1"
                    },
                )
                .is_some_and(|peer| {
                    matches!(
                        peer.handshake,
                        WireGuardHandshakeStatus::Ago { seconds }
                            if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
                    ) && matches!(peer.rtt, WireGuardRttStatus::Unavailable { .. })
                })
        });
        if machines.len() == 3 && silent && healthy_pair {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "silent edge and RTT-unavailable healthy pair did not appear: {machines:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn peer<'a>(
    machine: &'a NetworkStatusMachine,
    projection: &DataplaneProjection,
    peer_machine: &str,
) -> Option<&'a ployz_core::dataplane::WireGuardPeerStatus> {
    let expected_key = &projection_member(projection, peer_machine).wireguard_public_key;
    let NetworkDataplaneTestimony::Answered { value } = &machine.dataplane else {
        return None;
    };
    value
        .wireguard
        .peers
        .iter()
        .find(|peer| &peer.public_key == expected_key)
}

fn projection_member<'a>(
    projection: &'a DataplaneProjection,
    machine: &str,
) -> &'a DataplaneProjectionMember {
    projection
        .declared_members()
        .iter()
        .find(|member| member.machine_id == machine_id(machine))
        .unwrap_or_else(|| panic!("projection omitted {machine}"))
}

async fn block_wireguard_pair(core: &CoreContext, left: &DindMachine, right: &DindMachine) {
    for (machine, destination) in [(left, right.bridge_ip), (right, left.bridge_ip)] {
        let destination = destination.to_string();
        let dropped = core
            .exec_on(
                machine,
                &[
                    "iptables",
                    "-I",
                    "OUTPUT",
                    "1",
                    "-p",
                    "udp",
                    "-d",
                    &destination,
                    "--dport",
                    "51820",
                    "-j",
                    "DROP",
                ],
            )
            .await;
        assert!(
            dropped.success(),
            "block WireGuard from {} to {destination} failed: {dropped:?}",
            machine.name
        );
    }
}

async fn reset_wireguard_peer(
    core: &CoreContext,
    machine: &DindMachine,
    peer: &DataplaneProjectionMember,
) {
    let removed = core
        .exec_on(
            machine,
            &[
                "wg",
                "set",
                "ployz-wg0",
                "peer",
                peer.wireguard_public_key.as_str(),
                "remove",
            ],
        )
        .await;
    assert!(
        removed.success(),
        "remove WireGuard peer failed: {removed:?}"
    );
    let Some(endpoint) = peer.mesh_endpoints.first() else {
        panic!("peer {:?} has no mesh endpoint", peer.machine_id);
    };
    let endpoint = endpoint.to_string();
    let subnet = peer.endpoint_subnet.as_string();
    let restored = core
        .exec_on(
            machine,
            &[
                "wg",
                "set",
                "ployz-wg0",
                "peer",
                peer.wireguard_public_key.as_str(),
                "endpoint",
                &endpoint,
                "allowed-ips",
                &subnet,
                "persistent-keepalive",
                "25",
            ],
        )
        .await;
    assert!(
        restored.success(),
        "restore WireGuard peer failed: {restored:?}"
    );
}

async fn wait_for_asymmetric_handshakes(
    core: &CoreContext,
    projection: &DataplaneProjection,
) -> Vec<NetworkStatusMachine> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let machines = network_status(core).await;
        let handshake = |machine: &str, remote: &str| {
            machines
                .iter()
                .find(|status| status.active.machine_id == machine_id(machine))
                .and_then(|status| peer(status, projection, remote))
                .map(|peer| peer.handshake)
        };
        let unhealthy = |machine, remote| match handshake(machine, remote) {
            Some(WireGuardHandshakeStatus::Never) => true,
            Some(WireGuardHandshakeStatus::Ago { seconds }) => {
                seconds > MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
            }
            None => false,
        };
        let fresh = |machine, remote| {
            matches!(
                handshake(machine, remote),
                Some(WireGuardHandshakeStatus::Ago { seconds })
                    if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS
            )
        };
        if machines.len() == 3
            && unhealthy("core_1", "edge_2")
            && unhealthy("edge_3", "core_1")
            && fresh("edge_2", "core_1")
            && fresh("edge_2", "edge_3")
            && fresh("core_1", "edge_3")
            && fresh("edge_3", "edge_2")
        {
            return machines;
        }
        assert!(
            Instant::now() < deadline,
            "asymmetric handshake graph did not appear: {machines:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn placement_target(namespace: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id(namespace),
        origin: None,
        services: vec![DeployServiceSpec {
            service_id: service_id("server"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("workload image reference"),
            image_source: ImageSource::Registry,
            replicas: ReplicaCount::try_new(2).expect("two replicas"),
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

async fn deploy_and_read_plan(
    core: &CoreContext,
    idempotency: &str,
    target: DeployRequest,
) -> DeployPlan {
    let accepted = core
        .api
        .deploy_submit(&reserved_deploy_request(core, idempotency, target).await)
        .await
        .expect("placement deploy submits");
    let status =
        wait_for_terminal_deploy_status(core, &accepted.operation_id, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "placement deploy did not complete: {status:?}"
    );
    let events = terminal_operation_events(core, &accepted.operation_id).await;
    let Some(OperationEvent::DeployPlanCreated { plan, .. }) = events
        .into_iter()
        .find(|event| matches!(event, OperationEvent::DeployPlanCreated { .. }))
    else {
        panic!("placement deploy did not record its plan");
    };
    plan
}

async fn runtime_machines(
    core: &CoreContext,
    namespace: &ployz_core::ids::NamespaceId,
) -> Vec<ployz_core::ids::MachineId> {
    let [edge_2, edge_3] = core.cluster.edges() else {
        panic!("placement scenario requires exactly two edges");
    };
    let namespace = namespace.as_str();
    let mut machines = Vec::new();
    for (machine_id, machine) in [
        (machine_id("core_1"), core.cluster.core()),
        (machine_id("edge_2"), edge_2),
        (machine_id("edge_3"), edge_3),
    ] {
        for container in managed_workload_containers(core, machine).await {
            if container
                .labels
                .get(NAMESPACE_ID_LABEL)
                .is_some_and(|label| label == namespace)
            {
                machines.push(machine_id.clone());
            }
        }
    }
    machines.sort();
    machines
}
