//! Control runtime against a secured (TLS + NKey-authorized) NATS server:
//! bootstrap, operation API serving, deploy submit/commit, routed deploys
//! through the gateway, and drift refusal.
//!
//! Machine-add credential minting has its own suite in
//! `machine_add_mint.rs`; the shared fixture lives in `support::control`.

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::NamespaceRevisionEntryId;
use ployz_core::install::{InstallArtifactVersion, MachineBootstrapUrl};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, OperationStatus, RouteTarget,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::security::NatsPrincipal;
use ployz_core::state::{
    ActiveMachineState, GatewayServingStatus, GatewayStatusObservation, MachinePublicIpObservation,
    RouteBindingState, ServingTargetEntry,
};
use ployz_core::subjects::OperationApiEndpoint;
use ployz_nats::connect::connect_authenticated;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operation_api_client::OperationApiClientError;
use ployz_sdk_types::{
    DeploySubmitRequest, MachineAddError, MachineAddRequest, MachineInspectRequest,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineListRequest, MachineUpdateError,
    MachineUpdateRequest, RuntimeDerivedCollectionStatus, RuntimeSnapshotRequest,
    ServiceInspectRequest, ServiceListRequest,
};
use ployz_test_support::ops::wait_for_terminal_status;
use ployzd::controllers::MachineAddBootstrapConfig;
use ployzd::gateway_process_runtime::start_gateway_process_runtime_with_client;
use ployzd::machine_runtime::service::start_machine_runtime_service;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use support::machine_runtime::{ObservingContainerRunner, ReadyWireGuardEbpf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

mod support;

use ployz_test_support::containers;
use ployz_test_support::ids::{
    event_sequence, idempotency_key, machine_id, namespace_id, namespace_revision_entry_id,
    namespace_revision_id, operation_id, route_hostname, route_port, service_id,
};
use support::control::{TestNats, machine_join_template, redeem_when_ready};

#[tokio::test]
async fn control_runtime_bootstraps_nats_and_serves_operation_api() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: operation_id("op_control_runtime"),
            target: deploy_target("svc_api"),
        })
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_control_runtime"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    nats.connected
        .jetstream
        .get_key_value("KV_CORE")
        .await
        .expect("control runtime created KV_CORE");
    nats.connected
        .jetstream
        .get_key_value("KV_OPS")
        .await
        .expect("control runtime created KV_OPS");
    nats.connected
        .jetstream
        .get_stream("PLZ_OPS")
        .await
        .expect("control runtime created PLZ_OPS");
    assert!(
        nats.connected
            .jetstream
            .get_object_store("PLZ_BACKUPS")
            .await
            .is_err()
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_does_not_mutate_machine_state_on_startup() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");

    assert!(
        core_state
            .active_machines()
            .await
            .expect("active machines read")
            .is_empty()
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_uses_configured_machine_bootstrap_url() {
    let nats = TestNats::start().await;
    let config = nats.control_config().with_machine_bootstrap(
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new("https://example.test/ployz.sh")
                .expect("valid bootstrap url"),
        )
        .with_join_material(
            machine_join_template(&nats),
            ployz_core::install::MachineJoinSecretDelivery {
                nats_credentials: nats.server().join_seed().clone(),
            },
        ),
    );
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let accepted = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine"),
            idempotency_key: idempotency_key("idem_machine"),
            machine_id: machine_id("machine_2"),
            name: ployz_sdk_types::MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
        })
        .await
        .expect("machine add succeeds");

    assert_eq!(
        accepted.bootstrap_url.as_str(),
        "https://example.test/ployz.sh"
    );
    let retry_error = api
        .machine_add(&MachineAddRequest {
            operation_id: operation_id("op_machine_retry"),
            idempotency_key: idempotency_key("idem_machine"),
            machine_id: machine_id("machine_2"),
            name: ployz_sdk_types::MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
        })
        .await
        .expect_err("duplicate machine add idempotency key is rejected");
    assert_eq!(
        retry_error,
        OperationApiClientError::Domain {
            endpoint: OperationApiEndpoint::MachineAdd,
            error: MachineAddError::DuplicateIdempotencyKey {
                operation_id: operation_id("op_machine_retry"),
            },
        }
    );

    let redeemed = redeem_when_ready(&api, &accepted.join_token).await;
    assert_eq!(redeemed.machine_id, machine_id("machine_2"));

    api.machine_join_report(&MachineJoinReportRequest {
        join_token: accepted.join_token.clone(),
        outcome: MachineJoinReportOutcome::Completed,
    })
    .await
    .expect("join completion reports");
    // The minted per-machine seed is a working Machine credential: connect
    // with it and publish this machine's observations.
    let minted_seed = ployz_core::nats_config::NatsUserSeed::try_new(
        redeemed.secret_delivery.nats_credentials.secret(),
    )
    .expect("minted material is a valid seed");
    let minted_client = connect_authenticated(
        &nats.server().config_with_seed(
            NatsPrincipal::Machine {
                machine_id: machine_id("machine_2"),
            },
            minted_seed,
        ),
        ployz_test_support::nats::TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("minted machine credential connects");
    let machine_jetstream = jetstream::new(minted_client);
    let observations = AsyncNatsObservationStore::from_jetstream(&machine_jetstream)
        .await
        .expect("open observations");
    observations
        .replace_machine_public_ip(&MachinePublicIpObservation {
            machine_id: machine_id("machine_2"),
            public_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2)),
        })
        .await
        .expect("public ip stores");
    observations
        .replace_gateway_status(&GatewayStatusObservation {
            machine_id: machine_id("machine_2"),
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            serving: GatewayServingStatus::Current,
            route_count: 0,
        })
        .await
        .expect("gateway status stores");

    let inspected = api
        .machine_inspect(&MachineInspectRequest {
            machine_id: machine_id("machine_2"),
        })
        .await
        .expect("machine inspects");
    assert_eq!(inspected.active.machine_id, machine_id("machine_2"));
    assert_eq!(inspected.active.name.as_str(), "edge_2");
    assert_eq!(
        inspected
            .public_ip
            .as_ref()
            .expect("public ip exists")
            .public_ip,
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2))
    );
    assert_eq!(
        inspected
            .gateway
            .as_ref()
            .expect("gateway status exists")
            .serving,
        GatewayServingStatus::Current
    );
    let machines = api
        .machine_list(&MachineListRequest {})
        .await
        .expect("machines list")
        .machines;
    assert_eq!(machines, vec![inspected]);

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_serves_active_service_queries() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_serving_target_entry(&ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_2"),
        })
        .await
        .expect("service state stores");
    let api = nats.api();

    let listed = api
        .service_list(&ServiceListRequest {})
        .await
        .expect("services list");
    let [service] = listed.services.as_slice() else {
        panic!("expected one listed service, got {:?}", listed.services);
    };
    assert_eq!(service.active.service_id, service_id("svc_api"));
    assert_eq!(
        service.active.namespace_revision_entry_id,
        namespace_revision_entry_id("entry_2")
    );

    let inspected = api
        .service_inspect(&ServiceInspectRequest {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("service inspects");
    assert_eq!(inspected.active.service_id, service_id("svc_api"));
    assert_eq!(
        inspected.active.namespace_revision_entry_id,
        namespace_revision_entry_id("entry_2")
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_serves_runtime_snapshot_projection() {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream::new(machine_client))
        .await
        .expect("open observations");
    core_state
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    core_state
        .replace_serving_target_entry(&ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_2"),
        })
        .await
        .expect("serving target entry stores");
    core_state
        .replace_route_binding(&RouteBindingState {
            namespace_id: namespace_id("default"),
            target: RouteTarget::new(route_hostname("api.example.com"), route_port(443)),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("route binding stores");
    observations
        .replace_machine_containers(&containers::snapshot(
            "machine_a",
            [containers::observation("machine_a", "ctr_api")
                .with(
                    containers::identity("svc_api")
                        .entry("entry_2")
                        .operation("op_deploy")
                        .step("step_run"),
                )
                .running_unroutable()],
        ))
        .await
        .expect("machine observations store");

    let snapshot = nats
        .api()
        .runtime_snapshot(&RuntimeSnapshotRequest {})
        .await
        .expect("runtime snapshot loads")
        .snapshot;

    assert_eq!(snapshot.machines.len(), 1);
    assert_eq!(snapshot.services.len(), 1);
    assert_eq!(snapshot.routes.len(), 1);
    assert_eq!(snapshot.containers.len(), 1);
    assert_eq!(
        snapshot.revisions,
        vec![ployz_sdk_types::RuntimeServiceRevision {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_2"),
        }]
    );
    assert_eq!(snapshot.releases.len(), 1);
    assert_eq!(snapshot.instances.len(), 1);
    assert_eq!(
        snapshot.projection_sources.revisions.status,
        RuntimeDerivedCollectionStatus::Complete
    );

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_refuses_machine_add_without_join_template() {
    let nats = TestNats::start().await;
    let result = ployzd::control_runtime::start_control_runtime_with_client_and_reload(
        nats.connected.controller.clone(),
        &nats.control_config_without_join_template(),
        nats.reload_runner(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ployzd::control_runtime::ControlRuntimeError::MissingMachineJoinTemplate)
    ));
}

#[tokio::test]
async fn control_runtime_runs_deploy_submit_and_commits_active_state() {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect_err("control has not bootstrapped observations yet");
    assert!(matches!(
        observations,
        ployz_nats::observations::ObservationStoreError::OpenBucket { .. }
    ));

    let config = nats
        .control_config()
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime = nats.start_control(&config).await;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let machine_jetstream = jetstream::new(machine_client.clone());
    let observations = AsyncNatsObservationStore::from_jetstream(&machine_jetstream)
        .await
        .expect("open observations");
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    let machine_runtime = start_machine_runtime_service(
        machine_client.clone(),
        machine_id("machine_a"),
        ObservingContainerRunner::new(machine_id("machine_a"), observations.clone()),
        ReadyWireGuardEbpf,
        ObservingContainerRunner::new(machine_id("machine_a"), observations.clone()),
    )
    .await
    .expect("machine runtime starts");
    let api = nats.api();
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_run"),
        target: deploy_target("svc_api"),
    };

    let accepted = api
        .deploy_submit(&request)
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_run"));
    let status =
        wait_for_terminal_status(&api, &operation_id("op_run"), Duration::from_secs(4)).await;
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
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    assert_eq!(
        core_state
            .serving_target_entry(&namespace_id("default"), &service_id("svc_api"))
            .await
            .expect("serving target entry reads")
            .expect("serving target committed")
            .namespace_revision_entry_id,
        deploy_target_entry_id("svc_api")
    );
    let duplicate = api
        .deploy_submit(&request)
        .await
        .expect("duplicate operation API submit returns original operation");
    assert_eq!(duplicate.operation_id, operation_id("op_run"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        observations
            .machine_snapshot(&machine_id("machine_a"))
            .await
            .expect("machine observations read")
            .expect("machine snapshot exists")
            .containers()
            .len(),
        1
    );

    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_rejects_current_machine_update() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let error = nats
        .api()
        .machine_update(&MachineUpdateRequest {
            operation_id: operation_id("op_update_core"),
            machine_id: machine_id("core_1"),
            target_version: InstallArtifactVersion::try_new("0.2.0")
                .expect("valid install version"),
        })
        .await
        .expect_err("current machine update is rejected");

    let OperationApiClientError::Domain {
        error: MachineUpdateError::CurrentMachineUnsupported { machine_id, .. },
        ..
    } = error
    else {
        panic!("unexpected machine update error: {error:?}");
    };
    assert_eq!(machine_id, self::machine_id("core_1"));

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_routed_deploy_serves_through_gateway() {
    let nats =
        TestNats::start_with_machines(&[machine_id("machine_a"), machine_id("machine_gateway")])
            .await;
    let config = nats
        .control_config()
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime = nats.start_control(&config).await;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    let machine_jetstream = jetstream::new(machine_client.clone());
    let observations = AsyncNatsObservationStore::from_jetstream(&machine_jetstream)
        .await
        .expect("open observations");
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.connected.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    observations
        .replace_machine_public_ip(&MachinePublicIpObservation {
            machine_id: machine_id("machine_a"),
            public_ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
        })
        .await
        .expect("machine public ip stores");
    let machine_runtime = start_machine_runtime_service(
        machine_client.clone(),
        machine_id("machine_a"),
        ObservingContainerRunner::new(machine_id("machine_a"), observations.clone()),
        ReadyWireGuardEbpf,
        ObservingContainerRunner::new(machine_id("machine_a"), observations.clone()),
    )
    .await
    .expect("machine runtime starts");
    let gateway_client = nats.machine_client(&machine_id("machine_gateway")).await;
    let gateway = start_gateway_process_runtime_with_client(
        gateway_client,
        Duration::from_millis(10),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        machine_id("machine_gateway"),
    )
    .await
    .expect("gateway runtime starts");
    let upstream = support::TestHttpUpstream::start("smoke").await;
    let api = nats.api();

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: operation_id("op_routed"),
            target: deploy_target_with_route(
                "svc_api",
                gateway.listen_addr().port(),
                upstream.port(),
            ),
        })
        .await
        .expect("operation API accepts routed deploy");

    assert_eq!(accepted.operation_id, operation_id("op_routed"));
    let status =
        wait_for_terminal_status(&api, &operation_id("op_routed"), Duration::from_secs(4)).await;
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
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ready = gateway.served_projection().is_some_and(|projection| {
            matches!(
                projection.routes.as_slice(),
                [route] if !route.upstreams.is_empty()
            )
        });
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gateway never served the routed projection; health: {:?}",
            gateway.health()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut client = TcpStream::connect(gateway.listen_addr())
        .await
        .expect("connect gateway");
    client
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com\r\nConnection: close\r\n\r\n")
        .await
        .expect("write gateway request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read gateway response");

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("\r\n\r\nsmoke"));
    let upstream_request = upstream.request().await;
    assert!(upstream_request.starts_with("GET /smoke HTTP/1.1\r\n"));
    assert!(upstream_request.contains("\r\nHost: api.example.com\r\n"));

    gateway.shutdown().await;
    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_refuses_bootstrap_resource_drift() {
    let nats = TestNats::start().await;
    nats.connected
        .jetstream
        .create_stream(jetstream::stream::Config {
            name: "PLZ_OPS".to_owned(),
            subjects: vec!["wrong.>".to_owned()],
            storage: StorageType::File,
            ..Default::default()
        })
        .await
        .expect("create drifted PLZ_OPS stream");

    let config = nats.control_config();
    let error = match ployzd::control_runtime::start_control_runtime_with_client_and_reload(
        nats.connected.controller.clone(),
        &config,
        nats.reload_runner(),
    )
    .await
    {
        Ok(runtime) => {
            runtime.shutdown().await.expect("unexpected runtime stops");
            panic!("control runtime should refuse drift");
        }
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ployzd::control_runtime::ControlRuntimeError::AssureBootstrap(
            ployz_nats::bootstrap::BootstrapAssuranceError::RefuseResource { .. }
        )
    ));
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn active_machine(value: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: machine_id(value),
        name: ployz_sdk_types::MachineName::try_new(value).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
        substrate_versions: None,
    }
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id("rev_2"),
        services: vec![DeployServiceSpec {
            service_id: self::service_id(service_id),
            image: image("ghcr.io/acme/api:rev-2"),
            replicas: replicas(1),
            routes: Vec::new(),
        }],
    }
}

fn deploy_target_entry_id(service_id: &str) -> NamespaceRevisionEntryId {
    ployz_core::deploy::namespace_revision_entry_id_for(
        &namespace_id("default"),
        &self::service_id(service_id),
        &image("ghcr.io/acme/api:rev-2"),
    )
}

fn deploy_target_with_route(
    service_id: &str,
    gateway_port: u16,
    endpoint_port: u16,
) -> DeployRequest {
    let mut target = deploy_target(service_id);
    let [service] = target.services.as_mut_slice() else {
        panic!("deploy target fixture has one service");
    };
    service.routes = vec![DeployRoute {
        target: RouteTarget::new(route_hostname("api.example.com"), route_port(gateway_port)),
        endpoint_port: route_port(endpoint_port),
    }];
    target
}
