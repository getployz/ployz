//! Control runtime against a secured (TLS + NKey-authorized) NATS server:
//! bootstrap, operation API serving, deploy submit/commit, routed deploys
//! through the gateway, and drift refusal.
//!
//! Machine-add credential minting has its own suite in
//! `machine_add_mint.rs`; the shared fixture lives in `support::control`.

use futures_util::StreamExt;
use ployz_core::cert::{ManagedLeaseIntent, PublicUrlMode};
use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec, ImageReference, ReplicaCount,
    VolumeName,
};
use ployz_core::ids::{MachineId, NamespaceRevisionEntryId};
use ployz_core::install::{InstallArtifactVersion, MachineBootstrapUrl};
use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, MachineSubstrateVersions, OperationStatus,
    RouteTarget,
};
use ployz_core::roles::InstallRolePolicy;
use ployz_core::security::NatsPrincipal;
use ployz_core::state::MachineLifecycle;
use ployz_core::state::{
    ActiveMachineState, GatewayServingStatus, GatewayStatusObservation, MachineEndpointObservation,
    RouteBindingState, VolumePinState,
};
use ployz_core::subjects::{
    MachineServiceEndpoint, OperationApiEndpoint, gateway_status,
    machine_facts as machine_facts_subject, machine_service,
};
use ployz_nats::connect::connect_authenticated;
use ployz_nats::operation_api_client::OperationApiClientError;
use ployz_nats::service_runtime::{NatsServiceResponse, RunningNatsService, start_nats_service};
use ployz_sdk_types::{
    DeploySubmitRequest, InitFirstMachineActivateRequest, MachineAddError, MachineAddRequest,
    MachineInspectRequest, MachineJoinReportOutcome, MachineJoinReportRequest, MachineListRequest,
    MachineTestimony, MachineUpdateError, MachineUpdateRequest, RuntimeDerivedCollectionStatus,
    RuntimeSnapshotRequest, ServiceInspectRequest, ServiceListRequest, VolumeListRequest,
    VolumeStatus,
};
use ployz_test_support::ops::wait_for_terminal_status;
use ployzd::intent::lease_intent::LeaseIntentStore;
use ployzd::intent::machine_roster::MachineRosterStore;
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::operation_api::admission::MachineAddBootstrapConfig;
use ployzd::roles::gateway::process::start_gateway_process_with_client;
use ployzd::roles::machine::protocol::{
    MachineFactsGetRpcOk, MachineFactsGetRpcResponse, MachineSubstrateReportRpcOk,
    MachineSubstrateReportRpcResponse, MachineSubstrateUpdateRpcOk,
    MachineSubstrateUpdateRpcResponse,
};
use ployzd::roles::machine::service::start_machine_role_service;
use ployzd::service_catalog::machine_role_service;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use support::machine_runtime::{ObservingContainerRunner, ReadyWireGuardEbpf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

mod support;

use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    event_sequence, idempotency_key, machine_id, namespace_id, namespace_revision_entry_id,
    operation_id, route_hostname, route_port, service_id,
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
            idempotency_key: idempotency_key("idem_control_runtime"),
            target: deploy_target("svc_api"),
        })
        .await
        .expect("operation API accepts deploy");

    assert!(accepted.operation_id.as_str().starts_with("op_deploy_"));
    assert_eq!(accepted.start_sequence, event_sequence(1));

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_does_not_mutate_machine_state_on_startup() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let machine_roster = machine_roster(&config).await;
    let runtime = nats.start_control(&config).await;

    assert!(
        machine_roster
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
async fn first_machine_reactivation_preserves_configured_public_url_mode() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let core_store = ployzd::core_store::CoreStore::open(config.core_db_path.clone())
        .await
        .expect("open core store");
    let machine_roster = MachineRosterStore::new(core_store.clone());
    let lease_intent = LeaseIntentStore::new(core_store);
    machine_roster
        .replace_active_machine(&active_machine("core_1"))
        .await
        .expect("active first machine stores");
    lease_intent
        .set_mode(PublicUrlMode::None)
        .await
        .expect("initial public URL mode stores");
    let runtime = nats.start_control(&config).await;

    nats.api()
        .init_first_machine_activate(&InitFirstMachineActivateRequest {
            machine_id: machine_id("core_1"),
            roles: InstallRolePolicy::install_all(),
            public_url_mode: PublicUrlMode::Auto,
        })
        .await
        .expect("first-machine reactivation succeeds");

    assert_eq!(
        lease_intent.load().await.expect("public URL mode loads"),
        ManagedLeaseIntent::None
    );
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn first_machine_reactivation_heals_missing_public_url_mode() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let core_store = ployzd::core_store::CoreStore::open(config.core_db_path.clone())
        .await
        .expect("open core store");
    let machine_roster = MachineRosterStore::new(core_store.clone());
    let lease_intent = LeaseIntentStore::new(core_store);
    machine_roster
        .replace_active_machine(&active_machine("core_1"))
        .await
        .expect("active first machine stores");
    let runtime = nats.start_control(&config).await;

    nats.api()
        .init_first_machine_activate(&InitFirstMachineActivateRequest {
            machine_id: machine_id("core_1"),
            roles: InstallRolePolicy::install_all(),
            public_url_mode: PublicUrlMode::None,
        })
        .await
        .expect("interrupted first-machine activation heals");

    assert_eq!(
        lease_intent.load().await.expect("public URL mode loads"),
        ManagedLeaseIntent::None
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
    let join_api = nats.join_api();

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

    let redeemed = redeem_when_ready(&join_api, &accepted.join_token).await;
    assert_eq!(redeemed.machine_id, machine_id("machine_2"));

    join_api
        .machine_join_report(&MachineJoinReportRequest {
            join_token: accepted.join_token.clone(),
            outcome: MachineJoinReportOutcome::Completed,
        })
        .await
        .expect("join completion reports");
    // The minted per-machine seed is a working Machine credential: connect
    // with it and publish this machine's facts.
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
    let _facts = start_facts_subscription(
        minted_client.clone(),
        machine_id("machine_2"),
        empty_machine_snapshot("machine_2"),
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2))),
    )
    .await;
    publish_gateway_status(
        &minted_client,
        GatewayStatusObservation {
            machine_id: machine_id("machine_2"),
            listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            serving: GatewayServingStatus::Current,
            route_count: 0,
        },
    )
    .await;

    let inspected = api
        .machine_inspect(&MachineInspectRequest {
            machine_id: machine_id("machine_2"),
        })
        .await
        .expect("machine inspects");
    assert_eq!(inspected.active.machine_id, machine_id("machine_2"));
    assert_eq!(inspected.active.name.as_str(), "edge_2");
    let MachineTestimony::Answered {
        endpoints, gateway, ..
    } = &inspected.testimony
    else {
        panic!("machine answered");
    };
    assert_eq!(
        endpoints
            .as_ref()
            .expect("endpoints exist")
            .control_endpoints
            .first()
            .copied(),
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2)))
    );
    assert_eq!(
        gateway.as_ref().expect("gateway status exists").serving,
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
    let namespace_intent = NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open(config.core_db_path.clone())
            .await
            .expect("open core store"),
    );
    let runtime = nats.start_control(&config).await;
    let mut serving_target = serving_target_entry("svc_api", "entry_2");
    serving_target.volume_names = vec![VolumeName::try_new("data").expect("valid volume name")];
    namespace_intent
        .replace_serving_target_entry(serving_target)
        .await
        .expect("service state stores");
    namespace_intent
        .replace_volume_pin(VolumePinState {
            namespace_id: namespace_id("default"),
            volume_name: VolumeName::try_new("data").expect("valid volume name"),
            machine_id: machine_id("core_1"),
        })
        .await
        .expect("volume pin stores");
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

    let volumes = api
        .volume_list(&VolumeListRequest {})
        .await
        .expect("volumes list");
    let [volume] = volumes.volumes.as_slice() else {
        panic!("expected one listed volume, got {:?}", volumes.volumes);
    };
    assert_eq!(volume.volume_name.as_str(), "data");
    assert_eq!(volume.status, VolumeStatus::InUse);

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
    let namespace_intent = NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open(config.core_db_path.clone())
            .await
            .expect("open core store"),
    );
    let machine_roster = machine_roster(&config).await;
    let runtime = nats.start_control(&config).await;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    machine_roster
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    namespace_intent
        .replace_serving_target_entry(serving_target_entry("svc_api", "entry_2"))
        .await
        .expect("serving target entry stores");
    namespace_intent
        .replace_route_binding(RouteBindingState {
            namespace_id: namespace_id("default"),
            target: RouteTarget::new(route_hostname("api.example.com"), route_port(443)),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("route binding stores");
    let _facts = start_facts_subscription(
        machine_client.clone(),
        machine_id("machine_a"),
        containers::snapshot(
            "machine_a",
            [containers::observation("machine_a", "ctr_api")
                .with(
                    containers::identity("svc_api")
                        .entry("entry_2")
                        .operation("op_deploy")
                        .step("step_run"),
                )
                .running_unroutable()],
        ),
        None,
    )
    .await;

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
    let result = ployzd::roles::control::start_control_process_with_client_and_reload(
        nats.connected.controller.clone(),
        &nats.control_config_without_join_template(),
        nats.reload_runner(),
    )
    .await;

    assert!(matches!(
        result,
        Err(ployzd::roles::control::ControlProcessError::MissingMachineJoinTemplate)
    ));
}

#[tokio::test]
async fn control_runtime_runs_deploy_submit_and_commits_active_state() {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;

    let config = nats
        .control_config()
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let machine_roster = machine_roster(&config).await;
    let runtime = nats.start_control(&config).await;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    machine_roster
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let machine_runtime = start_machine_role_service(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner.clone(),
    )
    .await
    .expect("machine runtime starts");
    let api = nats.api();
    let request = DeploySubmitRequest {
        idempotency_key: idempotency_key("idem_run"),
        target: deploy_target("svc_api"),
    };

    let accepted = api
        .deploy_submit(&request)
        .await
        .expect("operation API accepts deploy");

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
        "expected deploy to complete, got {status:?}"
    );
    let namespace_intent = NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open(config.core_db_path.clone())
            .await
            .expect("open core store"),
    );
    assert_eq!(
        namespace_intent
            .load()
            .await
            .expect("namespace intent reads")
            .serving_target_entries
            .into_iter()
            .find(|entry| {
                entry.namespace_id == namespace_id("default")
                    && entry.service_id == service_id("svc_api")
            })
            .expect("serving target committed")
            .namespace_revision_entry_id,
        deploy_target_entry_id("svc_api")
    );
    let duplicate = api
        .deploy_submit(&request)
        .await
        .expect("duplicate operation API submit returns original operation");
    assert_eq!(duplicate.operation_id, accepted.operation_id);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runner.snapshot().containers().len(), 1);

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
async fn control_runtime_records_machine_update_without_mutating_roster_intent() {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let config = nats.control_config();
    let machine_roster = machine_roster(&config).await;
    let runtime = nats.start_control(&config).await;
    machine_roster
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    let target_version = InstallArtifactVersion::try_new("0.2.0").expect("valid install version");
    let machine_update_service = start_substrate_update_service(
        nats.machine_client(&machine_id("machine_a")).await,
        &machine_id("machine_a"),
        target_version.clone(),
    )
    .await;

    let accepted = nats
        .api()
        .machine_update(&MachineUpdateRequest {
            operation_id: operation_id("op_update_machine_a"),
            machine_id: machine_id("machine_a"),
            target_version: target_version.clone(),
        })
        .await
        .expect("machine update accepts");

    let status =
        wait_for_terminal_status(&nats.api(), &accepted.operation_id, Duration::from_secs(4)).await;
    let OperationStatus::MachineUpdate {
        state: ployz_core::ops::MachineUpdateOperationState::Completed { reported },
        ..
    } = status
    else {
        panic!("expected completed machine update, got {status:?}");
    };
    assert_eq!(reported.ployzd, Some(target_version));
    assert_eq!(
        machine_roster
            .active_machine(&machine_id("machine_a"))
            .await
            .expect("active machine reads")
            .expect("machine remains active"),
        active_machine("machine_a"),
        "reported substrate versions are operation evidence and machine facts, not roster intent"
    );

    machine_update_service
        .shutdown()
        .await
        .expect("machine update service shuts down");
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
    let machine_roster = machine_roster(&config).await;
    let runtime = nats.start_control(&config).await;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    machine_roster
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let machine_runtime = start_machine_role_service(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf,
        runner.clone(),
    )
    .await
    .expect("machine runtime starts");
    let gateway_client = nats.machine_client(&machine_id("machine_gateway")).await;
    let gateway = start_gateway_process_with_client(
        gateway_client,
        Duration::from_millis(10),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        machine_id("machine_gateway"),
        None,
    )
    .await
    .expect("gateway runtime starts");
    let upstream = support::TestHttpUpstream::start("smoke").await;
    let api = nats.api();

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_routed"),
            target: deploy_target_with_route(
                "svc_api",
                gateway.listen_addr().port(),
                upstream.port(),
            ),
        })
        .await
        .expect("operation API accepts routed deploy");

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
    publish_machine_facts(
        &machine_client,
        runner.snapshot(),
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))),
    )
    .await;
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

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

async fn machine_roster(config: &ployzd::config::ControlProcessConfig) -> MachineRosterStore {
    MachineRosterStore::new(
        ployzd::core_store::CoreStore::open(config.core_db_path.clone())
            .await
            .expect("open core store"),
    )
}

fn active_machine(value: &str) -> ActiveMachineState {
    ActiveMachineState {
        control_endpoints: Vec::new(),
        mesh_endpoints: Vec::new(),
        lifecycle: MachineLifecycle::Active,
        machine_id: machine_id(value),
        name: ployz_sdk_types::MachineName::try_new(value).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
        endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new("10.198.0.0/24")
            .expect("valid endpoint subnet"),
    }
}

async fn start_facts_subscription(
    client: async_nats::Client,
    machine_id: MachineId,
    containers: MachineContainerObservationSnapshot,
    public_ip: Option<IpAddr>,
) -> tokio::task::JoinHandle<()> {
    let subject = machine_service(&machine_id, MachineServiceEndpoint::FactsGet);
    let mut subscriber = client
        .subscribe(subject)
        .await
        .expect("subscribe facts service");
    client
        .flush()
        .await
        .expect("flush facts service subscription");
    tokio::spawn(async move {
        while let Some(message) = subscriber.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            let facts = machine_facts(&machine_id, containers.clone(), public_ip);
            let response =
                serde_json::to_vec(&MachineFactsGetRpcResponse::Ok(MachineFactsGetRpcOk {
                    facts,
                }))
                .expect("facts response serializes");
            let _ = client.publish(reply, response.into()).await;
        }
    })
}

async fn publish_machine_facts(
    client: &async_nats::Client,
    containers: MachineContainerObservationSnapshot,
    public_ip: Option<IpAddr>,
) {
    let machine_id = containers.machine_id().clone();
    let facts = machine_facts(&machine_id, containers, public_ip);
    let payload = serde_json::to_vec(&facts).expect("machine facts encode");
    client
        .publish(machine_facts_subject(facts.machine_id()), payload.into())
        .await
        .expect("machine facts publish");
    client.flush().await.expect("flush machine facts");
}

async fn publish_gateway_status(client: &async_nats::Client, status: GatewayStatusObservation) {
    let payload = serde_json::to_vec(&status).expect("gateway status encodes");
    client
        .publish(gateway_status(&status.machine_id), payload.into())
        .await
        .expect("gateway status publishes");
    client.flush().await.expect("flush gateway status");
}

async fn start_substrate_update_service(
    client: async_nats::Client,
    machine_id: &MachineId,
    reported_ployzd: InstallArtifactVersion,
) -> RunningNatsService {
    let spec = machine_role_service(machine_id);
    let update_endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == machine_service(machine_id, MachineServiceEndpoint::SubstrateUpdate)
        })
        .expect("substrate.update endpoint exists")
        .clone();
    let report_endpoint = spec
        .endpoints
        .iter()
        .find(|endpoint| {
            endpoint.subject == machine_service(machine_id, MachineServiceEndpoint::SubstrateReport)
        })
        .expect("substrate.report endpoint exists")
        .clone();
    let machine_id = machine_id.clone();
    let mut service = start_nats_service(client.clone(), &spec)
        .await
        .expect("start substrate update service");
    service
        .bind_endpoint(&update_endpoint, {
            let machine_id = machine_id.clone();
            move |_request| {
                let machine_id = machine_id.clone();
                async move {
                    NatsServiceResponse::json_ok(&MachineSubstrateUpdateRpcResponse::Ok(
                        MachineSubstrateUpdateRpcOk { machine_id },
                    ))
                }
            }
        })
        .await
        .expect("bind substrate.update endpoint");
    service
        .bind_endpoint(&report_endpoint, move |_request| {
            let machine_id = machine_id.clone();
            let reported_ployzd = reported_ployzd.clone();
            async move {
                NatsServiceResponse::json_ok(&MachineSubstrateReportRpcResponse::Ok(
                    MachineSubstrateReportRpcOk {
                        machine_id,
                        reported: MachineSubstrateVersions {
                            ployzd: Some(reported_ployzd),
                            host_runner: None,
                        },
                    },
                ))
            }
        })
        .await
        .expect("bind substrate.report endpoint");
    client.flush().await.expect("flush substrate service");
    service
}

fn machine_facts(
    machine_id: &MachineId,
    containers: MachineContainerObservationSnapshot,
    public_ip: Option<IpAddr>,
) -> MachineFactsSnapshot {
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        containers,
        public_ip.map(|public_ip| MachineEndpointObservation {
            machine_id: machine_id.clone(),
            control_endpoints: vec![public_ip],
            mesh_endpoints: Vec::new(),
        }),
        test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts are valid")
}

fn test_disk_space() -> ployz_core::machine_runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
}

fn empty_machine_snapshot(value: &str) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(value), [])
        .expect("empty machine snapshot is valid")
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: self::service_id(service_id),
            image: image("ghcr.io/acme/api:rev-2"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: replicas(1),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            routes: Vec::new(),
        }],
    }
}

fn deploy_target_entry_id(service_id: &str) -> NamespaceRevisionEntryId {
    ployz_core::deploy::namespace_revision_entry_id_for(
        &namespace_id("default"),
        &self::service_id(service_id),
        &image("ghcr.io/acme/api:rev-2"),
        &ployz_core::deploy::ImageSource::Registry,
        &ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
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
        target: DeployRouteTarget::Hostname {
            hostname: route_hostname("api.example.com"),
            port: route_port(gateway_port),
        },
        endpoint_port: route_port(endpoint_port),
    }];
    target
}
