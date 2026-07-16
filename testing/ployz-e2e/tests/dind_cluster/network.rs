use std::time::{Duration, Instant};

use ployz_core::deploy::{DeployRequest, DeployRoute, DeployRouteTarget};
use ployz_core::ingress::AutomaticHostnameLabel;
use ployz_core::operation::{
    DeployCompletionOutcome, DeployOperationState, NetworkRepairOperationState, OperationStatus,
};
use ployz_core::security::NatsPrincipal;
use ployz_e2e::dind::{self, exec_in_container};
use ployz_nats::operation_api_client::{
    DEFAULT_OPERATION_API_REQUEST_TIMEOUT, OperationApiClientError,
};
use ployz_nats::service_runtime::NatsServiceRequestFailure;
use ployz_sdk_types::{
    DeployReserveRequest, NetworkDataplaneTestimony, NetworkInternalDnsTestimony,
    NetworkRepairRequest, NetworkResolveMachineTestimony, NetworkResolveRequest,
    NetworkStatusMachine, NetworkStatusRequest,
};
use ployz_test_support::ids::{machine_id, namespace_id, operation_id, route_port};
use ployz_test_support::ops::wait_for_terminal_status;

use super::{
    CONNECT_TIMEOUT, CoreContext, DEPLOY_TERMINAL_BUDGET, OVERLAY_FIRST_CONTACT_BUDGET,
    add_and_join_edge, assert_unit_active, committed_route_hostname, connect_core_client,
    cross_machine_internal_traffic_probe, finish, gateway_https_get_unverified, init_core_cluster,
    internal_dns_deploy_target, read_intent, reserved_deploy_request,
    wait_for_cross_machine_internal_traffic, wait_for_machine_observations,
    wait_for_ready_dataplane, wait_for_terminal_deploy_status, with_evidence,
};

/// Network observability is driven by intended membership, resolver answers
/// come from each bound resolver, and repair converges through a terminal
/// operation after a silent machine returns.
#[tokio::test]
async fn group_network_repair() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let [edge] = core.cluster.edges() else {
            panic!("scenario requires exactly one edge machine");
        };
        add_and_join_edge(&core, edge).await;
        wait_for_machine_observations(&core, &machine_id("core_1")).await;
        wait_for_machine_observations(&core, &machine_id("edge_2")).await;
        for machine in [core.cluster.core(), edge] {
            assert_unit_active(&core, machine, "ployzd-dns").await;
        }

        let controller = connect_core_client(
            &core,
            NatsPrincipal::Controller,
            &core.material.controller_seed,
        )
        .await
        .expect("connect controller for network intent");
        let intent = read_intent(&controller, CONNECT_TIMEOUT)
            .await
            .expect("read network intent");
        wait_for_ready_dataplane(&core, &intent.dataplane_projection).await;

        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            let status = core
                .api
                .network_status(&NetworkStatusRequest::First {
                    mode: ployz_sdk_types::NetworkStatusMode::Snapshot,
                })
                .await
                .expect("network status succeeds");
            if status.machines.len() == 2 && network_testimony_answered(&status.machines) {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "network testimony did not become ready: {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        assert_eq!(
            status.machines.len(),
            2,
            "status follows intended membership"
        );
        assert!(network_testimony_answered(&status.machines));

        let accepted = core
            .api
            .deploy_submit(
                &reserved_deploy_request(
                    &core,
                    "idem_dind_network_resolve",
                    routed_internal_dns_deploy_target(),
                )
                .await,
            )
            .await
            .expect("network resolve fixture deploy submits");
        let deploy =
            wait_for_terminal_deploy_status(&core, &accepted.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        assert!(
            matches!(
                &deploy,
                OperationStatus::Deploy {
                    state: DeployOperationState::Completed {
                        outcome: DeployCompletionOutcome::Completed,
                    },
                    ..
                }
            ),
            "network resolve fixture deploy did not complete: {deploy:?}"
        );
        let deadline = Instant::now() + OVERLAY_FIRST_CONTACT_BUDGET;
        let resolved = loop {
            let resolved = core
                .api
                .network_resolve(&NetworkResolveRequest {
                    name: "server.internal_dns.internal".to_owned(),
                })
                .await
                .expect("known network resolve succeeds");
            if resolved.machines.len() == 2
                && resolved.machines.iter().all(|testimony| {
                    matches!(
                        testimony,
                        NetworkResolveMachineTestimony::Answered { addresses, .. }
                            if addresses.len() == 1
                    )
                })
            {
                break resolved;
            }
            assert!(
                Instant::now() < deadline,
                "known service did not resolve on every machine: {resolved:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        let mut answer_sets = resolved.machines.iter().map(|testimony| {
            let NetworkResolveMachineTestimony::Answered { addresses, .. } = testimony else {
                unreachable!("loop accepts answered testimony only")
            };
            addresses
        });
        let Some(first) = answer_sets.next() else {
            panic!("known resolve omitted intended machines")
        };
        assert!(
            answer_sets.all(|addresses| addresses == first),
            "known service answers diverged: {resolved:?}"
        );
        let gateway_hostname =
            committed_route_hostname(&core, "internal_dns", "network fixture").await;

        let stopped = exec_in_container(
            &docker,
            &edge.container_id,
            &["systemctl", "stop", "ployzd-machine-edge_2", "ployzd-dns"],
        )
        .await;
        assert!(
            matches!(&stopped, Ok(outcome) if outcome.success()),
            "stopping edge testimony failed: {stopped:?}"
        );
        let silent_status = core
            .api
            .network_status(&NetworkStatusRequest::First {
                mode: ployz_sdk_types::NetworkStatusMode::Snapshot,
            })
            .await
            .expect("network status retains silent intended machine");
        let Some(silent_edge) = silent_status
            .machines
            .iter()
            .find(|machine| machine.active.machine_id == machine_id("edge_2"))
        else {
            panic!("silent intended edge was omitted: {silent_status:?}");
        };
        assert!(matches!(
            silent_edge.dataplane,
            NetworkDataplaneTestimony::NoAnswer
        ));
        assert!(matches!(
            silent_edge.internal_dns,
            NetworkInternalDnsTestimony::NoAnswer
        ));

        let restarted = exec_in_container(
            &docker,
            &edge.container_id,
            &["systemctl", "start", "ployzd-machine-edge_2", "ployzd-dns"],
        )
        .await;
        assert!(
            matches!(&restarted, Ok(outcome) if outcome.success()),
            "restarting edge testimony failed: {restarted:?}"
        );
        assert_unit_active(&core, edge, "ployzd-machine-edge_2").await;
        assert_unit_active(&core, edge, "ployzd-dns").await;

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = core
                .api
                .network_status(&NetworkStatusRequest::First {
                    mode: ployz_sdk_types::NetworkStatusMode::Snapshot,
                })
                .await
                .expect("network status after restart");
            if network_queries_ready(&status.machines) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "edge testimony did not recover: {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        let repair_id = operation_id("op_dind_network_repair");
        core.api
            .network_repair(&NetworkRepairRequest {
                operation_id: repair_id.clone(),
                machine_id: None,
            })
            .await
            .expect("network repair submits");
        let repaired =
            wait_for_terminal_status(&core.api, &repair_id, DEPLOY_TERMINAL_BUDGET).await;
        assert!(
            matches!(
                repaired,
                OperationStatus::NetworkRepair {
                    state: NetworkRepairOperationState::Completed,
                    ..
                }
            ),
            "network repair did not complete: {repaired:?}"
        );
        wait_for_ready_dataplane(&core, &intent.dataplane_projection).await;
        assert_last_known_good_survives_control_role_downtime(&core, &gateway_hostname).await;
        super::timed(
            "internal_service_dns",
            super::assert_internal_service_dns_reaches_cross_machine_sibling(&core),
        )
        .await;
    })
    .await;

    finish(core).await;
}

fn routed_internal_dns_deploy_target() -> DeployRequest {
    let mut target = internal_dns_deploy_target();
    let Some(server) = target
        .services
        .iter_mut()
        .find(|service| service.service_id.as_str() == "server")
    else {
        panic!("internal DNS fixture omits its server service")
    };
    server.routes.push(DeployRoute {
        target: DeployRouteTarget::AutoHostname {
            label: AutomaticHostnameLabel::try_new("network-failure")
                .expect("valid network failure route label"),
        },
        endpoint_port: route_port(80),
    });
    target
}

/// The control-plane-core outage stops `ployzd-control` while independently
/// supervised substrate and data-plane roles keep serving last-known-good.
async fn assert_last_known_good_survives_control_role_downtime(
    core: &CoreContext,
    gateway_hostname: &str,
) {
    let gateway_before = wait_for_gateway_response(core, gateway_hostname)
        .await
        .expect("Gateway route serves before ployzd-control downtime");
    assert!(
        gateway_before.contains("Welcome to nginx"),
        "Gateway route returned an unexpected response before ployzd-control downtime: {gateway_before}"
    );
    let internal_traffic = cross_machine_internal_traffic_probe(core)
        .await
        .expect("managed cross-machine client/server pair exists");
    wait_for_cross_machine_internal_traffic(core, &internal_traffic)
        .await
        .expect(
            "managed workload reaches its cross-machine sibling before ployzd-control downtime",
        );

    let stopped = exec_in_container(
        &core.docker,
        &core.cluster.core().container_id,
        &["systemctl", "stop", "ployzd-control"],
    )
    .await;
    assert!(
        matches!(&stopped, Ok(outcome) if outcome.success()),
        "stopping ployzd-control failed: {stopped:?}"
    );

    let request_budget = DEFAULT_OPERATION_API_REQUEST_TIMEOUT + Duration::from_secs(2);
    let request_started = Instant::now();
    let unavailable = tokio::time::timeout(
        request_budget,
        core.api.deploy_reserve(&DeployReserveRequest {
            namespace_id: namespace_id("control_unavailable"),
        }),
    )
    .await;
    let request_elapsed = request_started.elapsed();
    let gateway_during = wait_for_gateway_response(core, gateway_hostname).await;
    let internal_traffic_during =
        wait_for_cross_machine_internal_traffic(core, &internal_traffic).await;

    let restarted = exec_in_container(
        &core.docker,
        &core.cluster.core().container_id,
        &["systemctl", "start", "ployzd-control"],
    )
    .await;
    assert!(
        matches!(&restarted, Ok(outcome) if outcome.success()),
        "restarting ployzd-control failed: {restarted:?}"
    );
    assert_unit_active(core, core.cluster.core(), "ployzd-control").await;
    wait_for_machine_observations(core, &machine_id("core_1")).await;

    assert!(
        request_elapsed <= request_budget,
        "operation request exceeded its bounded failure budget: {request_elapsed:?}"
    );
    assert!(
        matches!(
            unavailable,
            Ok(Err(OperationApiClientError::Request {
                failure: NatsServiceRequestFailure::NoResponders
                    | NatsServiceRequestFailure::TimedOut,
                ..
            }))
        ),
        "operation request did not report typed ployzd-control unavailability: {unavailable:?}"
    );
    let gateway_during =
        gateway_during.expect("Gateway route remains available while ployzd-control is stopped");
    assert!(
        gateway_during.contains("Welcome to nginx"),
        "Gateway route returned an unexpected response while ployzd-control was stopped: {gateway_during}"
    );
    internal_traffic_during.expect(
        "managed workload internal DNS and traffic remain available while ployzd-control is stopped",
    );
}

async fn wait_for_gateway_response(core: &CoreContext, hostname: &str) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = Err(String::from("Gateway route was not queried"));
    while Instant::now() < deadline {
        last =
            gateway_https_get_unverified(core.cluster.core().published.gateway_tls, hostname).await;
        if matches!(&last, Ok(response) if response.contains("Welcome to nginx")) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    last
}

fn network_queries_ready(machines: &[NetworkStatusMachine]) -> bool {
    network_testimony_answered(machines)
        && machines
            .iter()
            .all(|machine| dns_resolver_is_serving(&machine.internal_dns))
}

fn network_testimony_answered(machines: &[NetworkStatusMachine]) -> bool {
    machines.iter().all(|machine| {
        matches!(
            machine.dataplane,
            NetworkDataplaneTestimony::Answered { .. }
        ) && matches!(
            machine.internal_dns,
            NetworkInternalDnsTestimony::Answered { .. }
        )
    })
}

fn dns_resolver_is_serving(testimony: &NetworkInternalDnsTestimony) -> bool {
    matches!(
        testimony,
        NetworkInternalDnsTestimony::Answered {
            value: ployz_sdk_types::InternalDnsStatus {
                resolver: ployz_sdk_types::InternalDnsResolverStatus::Serving { .. },
                ..
            }
        }
    )
}
