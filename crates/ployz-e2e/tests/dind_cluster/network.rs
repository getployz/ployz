use std::time::{Duration, Instant};

use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, NetworkRepairOperationState, OperationStatus,
};
use ployz_e2e::dind::{self, exec_in_container};
use ployz_sdk_types::{
    NetworkDataplaneTestimony, NetworkInternalDnsTestimony, NetworkRepairRequest,
    NetworkResolveMachineTestimony, NetworkResolveRequest, NetworkStatusMachine,
    NetworkStatusRequest,
};
use ployz_test_support::ids::{machine_id, operation_id};
use ployz_test_support::ops::wait_for_terminal_status;

use super::{
    DEPLOY_TERMINAL_BUDGET, OVERLAY_FIRST_CONTACT_BUDGET, add_and_join_edge, assert_unit_active,
    finish, init_core_cluster, internal_dns_deploy_target, reserved_deploy_request,
    wait_for_machine_observations, wait_for_terminal_deploy_status, with_evidence,
};

/// Network observability is driven by intended membership, resolver answers
/// come from each bound resolver, and repair converges through a terminal
/// operation after a silent machine returns.
#[tokio::test]
async fn scenario_network_status_resolve_and_repair() {
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

        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            let status = core
                .api
                .network_status(&NetworkStatusRequest::First {
                    mode: ployz_sdk_types::NetworkStatusMode::Snapshot,
                })
                .await
                .expect("network status succeeds");
            if status.machines.len() == 2 && network_queries_ready(&status.machines) {
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
        assert!(network_queries_ready(&status.machines));

        let resolved = core
            .api
            .network_resolve(&NetworkResolveRequest {
                name: "missing.default.internal".to_owned(),
            })
            .await
            .expect("network resolve succeeds");
        assert_eq!(resolved.machines.len(), 2);
        assert!(
            resolved.machines.iter().all(|testimony| matches!(
                testimony,
                NetworkResolveMachineTestimony::Answered { addresses, .. } if addresses.is_empty()
            )),
            "unexpected resolver testimony: {resolved:?}"
        );

        let accepted = core
            .api
            .deploy_submit(
                &reserved_deploy_request(
                    &core,
                    "idem_dind_network_resolve",
                    internal_dns_deploy_target(),
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
    })
    .await;

    finish(core).await;
}

fn network_queries_ready(machines: &[NetworkStatusMachine]) -> bool {
    machines.iter().all(|machine| {
        matches!(
            machine.dataplane,
            NetworkDataplaneTestimony::Answered { .. }
        ) && dns_resolver_is_serving(&machine.internal_dns)
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

#[test]
fn network_query_readiness_requires_bound_dns_resolver() {
    let testimony = |resolver| NetworkInternalDnsTestimony::Answered {
        value: ployz_sdk_types::InternalDnsStatus {
            resolver,
            fact_watermarks: Vec::new(),
        },
    };

    assert!(!dns_resolver_is_serving(&testimony(
        ployz_sdk_types::InternalDnsResolverStatus::AwaitingBind { attempts: 1 }
    )));
    assert!(dns_resolver_is_serving(&testimony(
        ployz_sdk_types::InternalDnsResolverStatus::Serving {
            bound: "10.198.1.1:53".parse().expect("resolver address")
        }
    )));
}
