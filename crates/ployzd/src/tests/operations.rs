//! Daemon-owned in-process operation scenarios over real NATS.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use crate::certificate::{AcmeIssueContext, AcmeIssuer, AcmeIssuerError, IssuedCertificate};
use crate::config::ControlProcessConfig;
use crate::control::intent::machine_roster::MachineRosterStore;
use crate::control::operations::deploy::{
    MachineContainerRuntime, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
};
use crate::control::role_client::machine::NatsMachineContainerRuntime;
use crate::control::sequencer::MachineAddBootstrapConfig;
use crate::control::store::CoreStore;
use crate::roles::gateway::process::start_gateway_process_with_client;
use crate::roles::machine::protocol::{
    MachineContainerRunRpcRequest, MachineDataplaneStatusRpcRequest,
    MachineDataplaneStatusRpcResponse, MachineRpcResponse,
};
use crate::roles::machine::service::{
    MachineServiceError, RunningMachineRoleRuntime, start_machine_role_runtime,
    start_machine_role_runtime_with_endpoint_observation,
};
use async_trait::async_trait;
use ployz::api_client::OperationApiClient;
use ployz_core::deploy::{
    DeployPlanningInput, DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec,
    ImageReference, ReplicaCount, plan_namespace_deploy,
};
use ployz_core::ids::OperationId;
use ployz_core::install::MachineBootstrapUrl;
use ployz_core::intent::ActiveMachineState;
use ployz_core::machine::MachineName;
use ployz_core::machine::roles::InstallRolePolicy;
use ployz_core::machine::runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::machine::{MachineEndpointObservation, MachineLifecycle};
use ployz_core::operation::{
    DeployCompletionOutcome, DeployOperationState, DeployPhaseNumber, DeployPhaseOutcome,
    DeployRunningStage, DeployServiceResult, EventSequence, OperationEvent,
    OperationEventReplayCursor, OperationEventReplayRequest, OperationStatus,
};
use ployz_nats::operation_api_client::OperationApiClientError;
use ployz_nats::service_runtime::request_json;
use ployz_nats::subjects::{MachineServiceEndpoint, machine_facts, machine_service};
use ployz_sdk_types::{
    DeployReserveRequest, DeploySubmitError, DeploySubmitRequest, OpsStatusRequest,
    ServiceInspectRequest,
};

mod credential_grant;
mod machine_lifecycle;
mod ops_list;
mod service_contract;

use crate::tests::support::machine_runtime::{
    ObservingContainerRunner, ReadyWireGuardEbpf, test_wireguard_public_key,
};
use ployz_test_support::ops::wait_for_terminal_status;

use crate::tests::support::http::{
    TestUpstream, free_gateway_http_port, http_get_with_host, https_get_with_host,
};
use crate::tests::support::nats::TestNats;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    event_replay_limit, event_sequence, idempotency_key, machine_id, namespace_id, route_hostname,
    route_port, service_id,
};

#[tokio::test]
async fn e2e_operations_over_real_nats() -> Result<(), Box<dyn Error + Send + Sync>> {
    e2e_deploy_submit_service_accepts_operation_over_real_nats().await?;

    Ok(())
}

async fn e2e_deploy_submit_service_accepts_operation_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[]).await;
    let config = nats
        .control_config(machine_id("core_1"))
        .with_machine_bootstrap(machine_bootstrap_config());
    let _runtime = nats.start_control(&config).await?;
    let api = OperationApiClient::new(nats.user_client());
    let request = reserved_deploy_request(&api, "idem_api_123", deploy_target("svc_api")).await?;
    let reservation_id = request.reservation_id;

    let accepted = api.deploy_submit(&request).await?;

    assert!(accepted.operation_id.as_str().starts_with("op_deploy_"));
    assert_eq!(
        accepted.watch_subject,
        format!(
            "plz.v1.progress.namespace.default.operation.{}.>",
            accepted.operation_id.as_str()
        )
    );
    assert_eq!(accepted.start_sequence, event_sequence(1));
    // The control runtime starts executing the accepted deploy immediately,
    // so status may have advanced past Accepted by the time we read it. The
    // acceptance contract is: status is reachable for the id and the first
    // durable event is the submission.
    let status_request = OpsStatusRequest {
        operation_id: accepted.operation_id.clone(),
    };
    let status = api.ops_status(&status_request).await?;
    let OperationStatus::Deploy {
        id,
        service_id: status_service_id,
        ..
    } = status.status
    else {
        panic!("submitted deploy should report a deploy status");
    };
    assert_eq!(id, accepted.operation_id);
    assert_eq!(status_service_id, service_id("svc_api"));

    let watch_request = OperationEventReplayRequest {
        operation_id: accepted.operation_id.clone(),
        start_sequence: event_sequence(1),
        limit: event_replay_limit(10),
    };
    let page = api.ops_watch(&watch_request).await?;
    let [first, ..] = page.events.as_slice() else {
        panic!("watch from sequence 1 should replay the submission event");
    };
    assert_eq!(
        first.event,
        OperationEvent::DeploySubmitted {
            operation_id: accepted.operation_id,
            reservation_id: Some(reservation_id),
            target: deploy_target("svc_api"),
        }
    );

    Ok(())
}

#[tokio::test]
async fn e2e_newer_deploy_reservation_fences_older_submit_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[]).await;
    let config = nats
        .control_config(machine_id("core_1"))
        .with_machine_bootstrap(machine_bootstrap_config());
    let _runtime = nats.start_control(&config).await?;
    let api = OperationApiClient::new(nats.user_client());
    let namespace_id = namespace_id("default");
    let older = api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: namespace_id.clone(),
        })
        .await?;
    let newer = api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: namespace_id.clone(),
        })
        .await?;

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            registry_credentials: std::collections::BTreeMap::new(),
            idempotency_key: idempotency_key("idem_newer"),
            reservation_id: newer.reservation_id,
            target: deploy_target("svc_api"),
        })
        .await?;
    wait_for_terminal_status(&api, &accepted.operation_id, Duration::from_secs(4)).await;

    let error = api
        .deploy_submit(&DeploySubmitRequest {
            registry_credentials: std::collections::BTreeMap::new(),
            idempotency_key: idempotency_key("idem_older"),
            reservation_id: older.reservation_id,
            target: deploy_target("svc_api"),
        })
        .await
        .expect_err("older reservation is fenced after the newer submit");

    assert!(matches!(
        error,
        OperationApiClientError::Domain {
            error: DeploySubmitError::StaleReservation {
                namespace_id: rejected_namespace,
                reservation_id,
                last_committed_reservation_id,
                ..
            },
            ..
        } if rejected_namespace == namespace_id
            && reservation_id == older.reservation_id
            && last_committed_reservation_id == newer.reservation_id
    ));

    Ok(())
}

#[tokio::test]
async fn e2e_control_and_machine_complete_deploy_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime = start_control_with_deploy_roster(&nats, &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let machine_runtime = start_machine_role_runtime(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id("machine_a")),
        runner.clone(),
    )
    .await?;
    wait_for_dataplane_projection(&nats, &machine_id("machine_a")).await;
    let api = OperationApiClient::new(nats.user_client());
    let request = reserved_deploy_request(&api, "idem_e2e_run", deploy_target("svc_api")).await?;

    let accepted = api.deploy_submit(&request).await?;
    let deploy_operation = accepted.operation_id.clone();

    let status = wait_for_terminal_status(&api, &deploy_operation, Duration::from_secs(4)).await;
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
        "expected deploy to complete, got {status:?}"
    );
    let requested_image = image("ghcr.io/acme/api:rev-2");
    let resolved_image = requested_image
        .with_digest(&ployz_core::image::OciDigest::sha256(
            requested_image.as_str().as_bytes(),
        ))
        .expect("resolved image reference is valid");
    let mut resolved_target = deploy_target("svc_api");
    let [resolved_service] = resolved_target.services.as_mut_slice() else {
        panic!("deploy target has one service");
    };
    resolved_service.image = resolved_image.clone();
    let resolved_service_target =
        ployz_core::deploy::NormalizedDeployRequest::try_new(resolved_target.clone())
            .expect("request normalizes")
            .services()
            .first()
            .expect("resolved deploy target has one service")
            .clone();
    assert_eq!(
        api.service_inspect(&ServiceInspectRequest {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
        })
        .await?
        .active
        .namespace_revision_entry_id,
        resolved_service_target.namespace_revision_entry_id()
    );
    assert_eq!(
        operation_events(&api, deploy_operation.clone(), accepted.start_sequence).await?,
        vec![
            OperationEvent::DeploySubmitted {
                operation_id: deploy_operation.clone(),
                reservation_id: Some(ployz_core::deploy::DeployReservationId::first()),
                target: deploy_target("svc_api"),
            },
            OperationEvent::DeployPlanningStarted {
                operation_id: deploy_operation.clone(),
            },
            OperationEvent::DeployImageResolved {
                operation_id: deploy_operation.clone(),
                service_id: service_id("svc_api"),
                machine_id: machine_id("machine_a"),
                requested: requested_image,
                resolved: resolved_image,
                credential_supplied: false,
            },
            OperationEvent::DeployPlanCreated {
                operation_id: deploy_operation.clone(),
                plan: plan_namespace_deploy(
                    namespace_id("default"),
                    resolved_target.namespace_revision_id(),
                    vec![DeployPlanningInput {
                        request: resolved_service_target,
                        eligible_machines: vec![machine_id("machine_a")],
                        existing_replicas: Vec::new(),
                        cleanup_candidates: Vec::new(),
                        volume_pins: Vec::new(),
                    }],
                    Vec::new(),
                )
                .expect("single-machine deploy plan is valid"),
            },
            OperationEvent::DeployRunning {
                operation_id: deploy_operation.clone(),
                stage: DeployRunningStage::StartingContainers,
            },
            OperationEvent::DeployPhaseStarted {
                operation_id: deploy_operation.clone(),
                phase: DeployPhaseNumber::try_new(1).expect("positive phase number"),
                service_ids: vec![service_id("svc_api")],
            },
            OperationEvent::DeployContainerStarted {
                operation_id: deploy_operation.clone(),
                machine_id: machine_id("machine_a"),
                container_id: ployz_core::ids::ContainerId::try_new("ctr_1")
                    .expect("valid container id"),
            },
            OperationEvent::DeployRunning {
                operation_id: deploy_operation.clone(),
                stage: DeployRunningStage::WaitingForHealth,
            },
            OperationEvent::DeployHealthCheckStarted {
                operation_id: deploy_operation.clone(),
            },
            OperationEvent::DeployRunning {
                operation_id: deploy_operation.clone(),
                stage: DeployRunningStage::ServingTargetCommit,
            },
            OperationEvent::DeployPhaseFinished {
                operation_id: deploy_operation.clone(),
                phase: DeployPhaseNumber::try_new(1).expect("positive phase number"),
                outcome: DeployPhaseOutcome::Promoted,
                services: vec![DeployServiceResult::Completed {
                    service_id: service_id("svc_api"),
                }],
            },
            OperationEvent::DeployCompleted {
                operation_id: deploy_operation,
                outcome: DeployCompletionOutcome::Completed,
            },
        ]
    );
    assert_eq!(runner.snapshot().containers().len(), 1);

    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_routed_deploy_serves_http_through_gateway() -> Result<(), Box<dyn Error + Send + Sync>>
{
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime = start_control_with_deploy_roster(&nats, &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let machine_runtime = start_routed_machine_role_runtime(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
    )
    .await?;
    wait_for_dataplane_projection(&nats, &machine_id("machine_a")).await;
    let gateway_http_port = free_gateway_http_port().await?;
    let gateway_runtime = start_gateway_process_with_client(
        machine_client.clone(),
        Duration::from_millis(10),
        format!("127.0.0.1:{gateway_http_port}").parse()?,
        machine_id("machine_a"),
        None,
    )
    .await?;
    let upstream = TestUpstream::start().await;
    let api = OperationApiClient::new(nats.user_client());
    let request = reserved_deploy_request(
        &api,
        "idem_e2e_route",
        deploy_target_with_route("svc_api", "localhost", upstream.port()),
    )
    .await?;

    let accepted = api.deploy_submit(&request).await?;

    let status =
        wait_for_terminal_status(&api, &accepted.operation_id, Duration::from_secs(4)).await;
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
        "expected routed deploy to complete, got {status:?}"
    );
    assert!(
        accepted.operation_id.as_str().starts_with("op_deploy_"),
        "deploy operation ids are server-generated, got {}",
        accepted.operation_id.as_str()
    );
    publish_machine_facts(&machine_client, runner.snapshot(), Some(public_ip(7))).await;
    wait_for_gateway_upstream(&gateway_runtime, "127.0.0.1", upstream.port()).await;

    assert_redirect_response(
        &http_get_with_host(gateway_runtime.listen_addr(), "localhost").await?,
    );
    assert_smoke_response(
        &https_get_with_host(gateway_runtime.tls_listen_addr(), "localhost").await?,
    );
    let upstream_request = upstream.request().await;
    assert!(upstream_request.starts_with("GET /smoke HTTP/1.1\r\n"));
    assert!(upstream_request.contains("\r\nHost: localhost\r\n"));

    gateway_runtime
        .shutdown()
        .await
        .expect("gateway runtime shuts down");
    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_gateway_serves_route_after_machine_runtime_shutdown()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let client = nats.controller_client();
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime = start_control_with_deploy_roster(&nats, &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let machine_runtime = start_routed_machine_role_runtime(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
    )
    .await?;
    wait_for_dataplane_projection(&nats, &machine_id("machine_a")).await;
    let gateway_http_port = free_gateway_http_port().await?;
    let gateway_runtime = start_gateway_process_with_client(
        machine_client.clone(),
        Duration::from_millis(10),
        format!("127.0.0.1:{gateway_http_port}").parse()?,
        machine_id("machine_a"),
        None,
    )
    .await?;
    let upstream = TestUpstream::start_with_expected_requests(2).await;
    let api = OperationApiClient::new(nats.user_client());
    let route_port = route_port(gateway_runtime.listen_addr().port());
    let route_host = format!("localhost:{}", route_port.get());
    let request = reserved_deploy_request(
        &api,
        "idem_e2e_machine_runtime_down_route",
        deploy_target_with_route("svc_api", "localhost", upstream.port()),
    )
    .await?;

    let accepted = api.deploy_submit(&request).await?;

    let status =
        wait_for_terminal_status(&api, &accepted.operation_id, Duration::from_secs(4)).await;
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
        "expected routed deploy to complete, got {status:?}"
    );
    publish_machine_facts(&machine_client, runner.snapshot(), Some(public_ip(7))).await;
    wait_for_gateway_upstream(&gateway_runtime, "127.0.0.1", upstream.port()).await;
    assert_smoke_response(
        &https_get_with_host(gateway_runtime.tls_listen_addr(), &route_host).await?,
    );

    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    let mut machine_rpc = NatsMachineContainerRuntime::new(client.clone())
        .with_request_timeout(Duration::from_millis(200));
    let unavailable = machine_rpc
        .run_container(&machine_id("machine_a"), machine_rpc_probe_request())
        .await
        .expect_err("machine service is unavailable after machine runtime shutdown");
    let MachineContainerRuntimeError::Unavailable {
        machine_id: id,
        reason,
    } = unavailable
    else {
        panic!("expected machine service unavailable, got {unavailable:?}");
    };
    assert_eq!(id, machine_id("machine_a"));
    // Right after shutdown either the subscription is already gone (NoResponders) or
    // the request outruns the unsubscribe and hits the timeout — both mean unavailable.
    assert!(
        matches!(
            reason,
            MachineRuntimeUnavailableReason::NoResponders
                | MachineRuntimeUnavailableReason::RequestTimedOut
        ),
        "expected NoResponders or RequestTimedOut, got {reason:?}"
    );
    assert_smoke_response(
        &https_get_with_host(gateway_runtime.tls_listen_addr(), &route_host).await?,
    );
    assert_eq!(upstream.requests().await.len(), 2);

    gateway_runtime
        .shutdown()
        .await
        .expect("gateway runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_gateway_keeps_serving_last_projection_after_control_shutdown()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime = start_control_with_deploy_roster(&nats, &config).await?;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let machine_runtime = start_routed_machine_role_runtime(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
    )
    .await?;
    wait_for_dataplane_projection(&nats, &machine_id("machine_a")).await;
    let gateway_http_port = free_gateway_http_port().await?;
    let gateway_runtime = start_gateway_process_with_client(
        machine_client.clone(),
        Duration::from_millis(10),
        format!("127.0.0.1:{gateway_http_port}").parse()?,
        machine_id("machine_a"),
        None,
    )
    .await?;
    let first_upstream = TestUpstream::start_with_expected_requests(2).await;
    let first_upstream_port = first_upstream.port();
    let api = OperationApiClient::new(nats.user_client());
    let route_hostname = route_hostname("localhost");
    let route_port = route_port(gateway_runtime.listen_addr().port());
    let request = reserved_deploy_request(
        &api,
        "idem_e2e_control_down_route",
        deploy_target_with_route("svc_api", route_hostname.as_str(), first_upstream_port),
    )
    .await?;

    let accepted = api.deploy_submit(&request).await?;

    let status =
        wait_for_terminal_status(&api, &accepted.operation_id, Duration::from_secs(4)).await;
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
        "expected routed deploy to complete, got {status:?}"
    );
    publish_machine_facts(&machine_client, runner.snapshot(), Some(public_ip(7))).await;
    wait_for_gateway_upstream(&gateway_runtime, "127.0.0.1", first_upstream_port).await;
    assert_smoke_response(
        &https_get_with_host(
            gateway_runtime.tls_listen_addr(),
            &format!("localhost:{}", route_port.get()),
        )
        .await?,
    );
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    assert_smoke_response(
        &https_get_with_host(
            gateway_runtime.tls_listen_addr(),
            &format!("localhost:{}", route_port.get()),
        )
        .await?,
    );
    for request in first_upstream.requests().await {
        assert!(request.starts_with("GET /smoke HTTP/1.1\r\n"));
        assert!(request.contains(&("Host: localhost:".to_owned() + &route_port.get().to_string())));
    }

    gateway_runtime
        .shutdown()
        .await
        .expect("gateway runtime shuts down");
    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");

    Ok(())
}

#[tokio::test]
async fn e2e_two_machine_routed_deploy_serves_through_both_gateways()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_with_machines(&[machine_id("core_1"), machine_id("edge_2")]).await;
    let route_port = free_gateway_http_port().await?;
    let config = nats
        .control_config(machine_id("core_1"))
        .with_deploy_machines(vec![machine_id("core_1"), machine_id("edge_2")])
        .with_deploy_step_timeout(Duration::from_secs(2))
        .with_machine_bootstrap(machine_bootstrap_config());
    let control_runtime = start_control_with_deploy_roster(&nats, &config).await?;
    let core_machine_client = nats.machine_client(&machine_id("core_1")).await;
    let edge_machine_client = nats.machine_client(&machine_id("edge_2")).await;
    let core_runner = ObservingContainerRunner::new(machine_id("core_1"));
    let edge_runner = ObservingContainerRunner::new(machine_id("edge_2"));
    let core_machine_runtime = start_routed_machine_role_runtime(
        core_machine_client.clone(),
        machine_id("core_1"),
        core_runner.clone(),
    )
    .await?;
    let edge_machine_runtime = start_routed_machine_role_runtime(
        edge_machine_client.clone(),
        machine_id("edge_2"),
        edge_runner.clone(),
    )
    .await?;
    wait_for_dataplane_projection(&nats, &machine_id("core_1")).await;
    wait_for_dataplane_projection(&nats, &machine_id("edge_2")).await;
    let core_gateway_runtime = start_gateway_process_with_client(
        core_machine_client.clone(),
        Duration::from_millis(10),
        format!("127.0.0.1:{route_port}").parse()?,
        machine_id("core_1"),
        None,
    )
    .await?;
    let edge_gateway_runtime = start_gateway_process_with_client(
        edge_machine_client.clone(),
        Duration::from_millis(10),
        format!("[::1]:{route_port}").parse()?,
        machine_id("edge_2"),
        None,
    )
    .await?;
    let upstream = TestUpstream::start_with_expected_requests(2).await;
    let api = OperationApiClient::new(nats.user_client());
    let request = reserved_deploy_request(
        &api,
        "idem_e2e_two_machine_route",
        deploy_target_with_route("svc_api", "localhost", upstream.port()),
    )
    .await?;

    let accepted = api.deploy_submit(&request).await?;
    let deploy_operation = accepted.operation_id.clone();

    let status = wait_for_terminal_status(&api, &deploy_operation, Duration::from_secs(4)).await;
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
        "expected two-machine routed deploy to complete, got {status:?}"
    );
    publish_machine_facts(
        &core_machine_client,
        core_runner.snapshot(),
        Some(public_ip(1)),
    )
    .await;
    publish_machine_facts(
        &edge_machine_client,
        edge_runner.snapshot(),
        Some(public_ip(2)),
    )
    .await;
    wait_for_gateway_upstream(&core_gateway_runtime, "127.0.0.1", upstream.port()).await;
    wait_for_gateway_upstream(&edge_gateway_runtime, "127.0.0.1", upstream.port()).await;
    assert_eq!(core_runner.snapshot().containers().len(), 1);
    assert_smoke_response(
        &https_get_with_host(
            core_gateway_runtime.tls_listen_addr(),
            &format!("localhost:{route_port}"),
        )
        .await?,
    );
    assert_smoke_response(
        &https_get_with_host(
            edge_gateway_runtime.tls_listen_addr(),
            &format!("localhost:{route_port}"),
        )
        .await?,
    );
    assert_eq!(upstream.requests().await.len(), 2);

    edge_gateway_runtime
        .shutdown()
        .await
        .expect("edge gateway runtime shuts down");
    core_gateway_runtime
        .shutdown()
        .await
        .expect("core gateway runtime shuts down");
    edge_machine_runtime
        .shutdown()
        .await
        .expect("edge machine runtime shuts down");
    core_machine_runtime
        .shutdown()
        .await
        .expect("core machine runtime shuts down");
    control_runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");

    Ok(())
}

async fn operation_events(
    api: &OperationApiClient,
    operation_id: OperationId,
    start_sequence: EventSequence,
) -> Result<Vec<OperationEvent>, Box<dyn Error + Send + Sync>> {
    let page = api
        .ops_watch(&OperationEventReplayRequest {
            operation_id,
            start_sequence,
            limit: event_replay_limit(32),
        })
        .await?;
    assert_eq!(page.cursor, OperationEventReplayCursor::Terminal);

    Ok(page.events.into_iter().map(|event| event.event).collect())
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

async fn reserved_deploy_request(
    api: &OperationApiClient,
    idempotency: &str,
    target: DeployRequest,
) -> Result<DeploySubmitRequest, Box<dyn Error + Send + Sync>> {
    let reservation = api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: target.namespace_id.clone(),
        })
        .await?;
    Ok(DeploySubmitRequest {
        registry_credentials: std::collections::BTreeMap::new(),
        idempotency_key: idempotency_key(idempotency),
        reservation_id: reservation.reservation_id,
        target,
    })
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            service_id: self::service_id(service_id),
            image: image("ghcr.io/acme/api:rev-2"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: replicas(1),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn deploy_target_with_route(service_id: &str, hostname: &str, endpoint_port: u16) -> DeployRequest {
    let mut target = deploy_target(service_id);
    let [service] = target.services.as_mut_slice() else {
        panic!("deploy target has one service");
    };
    service.routes = vec![DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname: route_hostname(hostname),
        },
        endpoint_port: self::route_port(endpoint_port),
    }];
    target
}

fn machine_rpc_probe_request() -> MachineContainerRunRpcRequest {
    MachineContainerRunRpcRequest {
        pull: crate::roles::machine::protocol::MachineImagePull::Registry {
            credential: None,
            reference: image("ghcr.io/acme/api:probe"),
        },
        runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
        container: containers::identity("svc_probe")
            .entry("rev_probe")
            .operation("op_probe")
            .step("step_probe")
            .build(),
    }
}

async fn wait_for_gateway_upstream(
    runtime: &crate::roles::gateway::process::RunningGatewayProcess,
    endpoint_ip: &str,
    endpoint_port: u16,
) {
    for _ in 0..200 {
        if runtime.served_projection().is_some_and(|projection| {
            projection.routes.iter().any(|route| {
                route.upstreams.iter().any(|upstream| {
                    upstream.address.ip().to_string() == endpoint_ip
                        && upstream.address.port() == endpoint_port
                })
            })
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("gateway upstream did not become visible");
}

async fn publish_machine_facts(
    client: &async_nats::Client,
    containers: MachineContainerObservationSnapshot,
    public_ip: Option<std::net::IpAddr>,
) {
    let machine_id = containers.machine_id().clone();
    let facts = MachineFactsSnapshot::try_new(
        machine_id.clone(),
        containers,
        public_ip.map(|public_ip| MachineEndpointObservation {
            machine_id: machine_id.clone(),
            control_endpoints: vec![public_ip],
            mesh_endpoints: vec![std::net::SocketAddr::new(
                public_ip,
                ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
            )],
        }),
        test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts are valid");
    client
        .publish(
            machine_facts(facts.machine_id()),
            serde_json::to_vec(&facts)
                .expect("machine facts encode")
                .into(),
        )
        .await
        .expect("machine facts publish");
    client.flush().await.expect("flush machine facts");
}

fn test_disk_space() -> ployz_core::machine::runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
}

fn public_ip(last_octet: u8) -> std::net::IpAddr {
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, last_octet))
}

async fn start_routed_machine_role_runtime(
    client: async_nats::Client,
    machine_id: ployz_core::ids::MachineId,
    runner: ObservingContainerRunner,
) -> Result<RunningMachineRoleRuntime, MachineServiceError> {
    let loopback = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    start_machine_role_runtime_with_endpoint_observation(
        client,
        machine_id.clone(),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id),
        runner,
        MachineEndpointObservation {
            machine_id,
            control_endpoints: vec![loopback],
            mesh_endpoints: Vec::new(),
        },
    )
    .await
}

fn assert_smoke_response(response: &str) {
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "unexpected response: {response:?}"
    );
    assert!(
        response.ends_with("\r\n\r\nsmoke"),
        "unexpected response body: {response:?}"
    );
}

fn assert_redirect_response(response: &str) {
    assert!(
        response.starts_with("HTTP/1.1 301 Moved Permanently\r\n"),
        "unexpected redirect response: {response:?}"
    );
    assert!(
        response.contains("location: https://localhost/smoke\r\n"),
        "unexpected redirect location: {response:?}"
    );
}

async fn start_control_with_deploy_roster(
    nats: &TestNats,
    config: &ControlProcessConfig,
) -> Result<crate::control::process::RunningControlProcess, Box<dyn Error + Send + Sync>> {
    let store = CoreStore::open(config.core_db_path.clone()).await?;
    let roster = MachineRosterStore::new(store);
    for (index, machine_id) in config.deploy_machines.iter().enumerate() {
        let endpoint_subnet = ployz_core::network::MachineEndpointSubnet::try_new(
            ployz_core::network::default_endpoint_subnet(machine_id),
        )?;
        roster
            .replace_active_machine(&ActiveMachineState {
                machine_id: machine_id.clone(),
                name: MachineName::try_new(format!("test_machine_{}", index + 1))?,
                activated_by: OperationId::try_new(format!("op_seed_machine_{}", index + 1))?,
                roles: InstallRolePolicy::install_all(),
                lifecycle: MachineLifecycle::Active,
                control_endpoints: Vec::new(),
                mesh_endpoints: vec![std::net::SocketAddr::new(
                    public_ip(u8::try_from(index + 1)?),
                    ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
                )],
                endpoint_subnet,
                wireguard_public_key: test_wireguard_public_key(machine_id),
            })
            .await?;
    }
    Ok(nats
        .start_control_with_test_issuer(config, Arc::new(FixtureAcmeIssuer))
        .await?)
}

struct FixtureAcmeIssuer;

#[async_trait]
impl AcmeIssuer for FixtureAcmeIssuer {
    async fn issue_http01(
        &self,
        context: &AcmeIssueContext,
        hostname: &ployz_core::operation::RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        let challenge = ployz_core::certificate::AcmeHttp01Challenge::try_new(
            hostname.clone(),
            ployz_core::certificate::AcmeChallengeToken::try_new("e2e-token")
                .expect("challenge token"),
            ployz_core::certificate::AcmeChallengeValue::try_new("e2e-token.fixture-thumbprint")
                .expect("challenge value"),
            ployz_core::certificate::AcmeChallengeTtlSeconds::try_new(900).expect("challenge ttl"),
        )
        .expect("challenge");
        context.publish_challenge(challenge).await?;
        context.validation_started().await?;
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed([hostname.as_str().to_owned()]).map_err(
                |error| AcmeIssuerError::Validation {
                    message: error.to_string(),
                },
            )?;
        Ok(IssuedCertificate {
            certificate_chain_pem: cert.pem(),
            private_key_pem: signing_key.serialize_pem(),
        })
    }
}

async fn wait_for_dataplane_projection(nats: &TestNats, machine_id: &ployz_core::ids::MachineId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let response = request_json::<_, MachineDataplaneStatusRpcResponse>(
            &nats.controller_client(),
            machine_service(machine_id, MachineServiceEndpoint::DataplaneStatus),
            &MachineDataplaneStatusRpcRequest {
                mode: ployz_core::network::NetworkStatusMode::Snapshot,
            },
            Duration::from_millis(250),
        )
        .await;
        if matches!(
            response,
            Ok(MachineRpcResponse::Ok(ok))
                if matches!(
                    ok.value.projection.testimony,
                    ployz_core::network::DataplaneProjectionTestimony::Applied { .. }
                )
        ) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "machine {machine_id:?} did not converge its dataplane projection"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn machine_bootstrap_config() -> MachineAddBootstrapConfig {
    MachineAddBootstrapConfig::new(
        MachineBootstrapUrl::try_new(ployz_core::install::DEFAULT_MACHINE_BOOTSTRAP_URL)
            .expect("valid bootstrap url"),
    )
    .with_join_material(
        ployz_test_support::fixtures::machine_join_template(),
        ployz_core::install::MachineJoinSecretDelivery {
            nats_credentials: ployz_core::nats_config::NatsUserSeed::try_new(
                "SUAIZ5LKGG2Y4WC7ZPKS46LSLLJQIFTO6KMSWSU2VN3TC7YRRIKH5WRXJQ",
            )
            .expect("valid seed"),
        },
    )
}
