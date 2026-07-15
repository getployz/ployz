//! Control runtime against a secured (TLS + NKey-authorized) NATS server:
//! bootstrap, operation API serving, deploy submit/commit, routed deploys
//! through the gateway, and drift refusal.
//!
//! Machine-add credential minting has its own scenario module; the shared
//! fixture lives in `crate::tests::support::control`.

use crate::certificate::{AcmeIssueContext, AcmeIssuer, AcmeIssuerError, IssuedCertificate};
use crate::control::intent::machine_roster::MachineRosterStore;
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::sequencer::MachineAddBootstrapConfig;
use crate::lease::LeaseWorkerUrl;
use crate::roles::gateway::process::start_gateway_process_with_client;
use crate::roles::machine::protocol::{
    MachineDataplaneStatusRpcRequest, MachineDataplaneStatusRpcResponse, MachineFactsGetRpcOk,
    MachineFactsGetRpcResponse, MachineRpcResponse, MachineSubstrateReportRpcOk,
    MachineSubstrateReportRpcResponse, MachineSubstrateUpdateRpcOk,
    MachineSubstrateUpdateRpcResponse,
};
use crate::roles::machine::service::{
    start_machine_role_runtime, start_machine_role_runtime_with_endpoint_observation,
};
use crate::service_catalog::machine_role_service;
use crate::tests::support::machine_runtime::{ObservingContainerRunner, ReadyWireGuardEbpf};
use futures_util::StreamExt;
use ployz_core::deploy::{
    DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec, ImageReference,
    RegistryCredential, ReplicaCount, VolumeName,
};
use ployz_core::ids::{MachineId, RouteBindingId};
use ployz_core::image::OciDigest;
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::install::{InstallArtifactVersion, MachineBootstrapUrl};
use ployz_core::intent::{ActiveMachineState, RouteBindingState, VolumePinState};
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::roles::InstallRolePolicy;
use ployz_core::machine::runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::machine::{
    GatewayServingStatus, GatewayStatusObservation, MachineEndpointObservation,
};
use ployz_core::operation::{
    DeployCompletionOutcome, DeployOperationState, MachineSubstrateVersions, OperationEvent,
    OperationEventReplayLimit, OperationStatus, RouteTarget,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::connect_authenticated;
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_nats::service_runtime::{
    NatsServiceResponse, RunningNatsService, request_json, start_nats_service,
};
use ployz_nats::subjects::{
    INTENT_CHANGED, MachineServiceEndpoint, OperationApiEndpoint, RUNTIME_SNAPSHOT_SEED,
    RUNTIME_SNAPSHOT_STREAM, gateway_status, machine_facts as machine_facts_subject,
    machine_service,
};
use ployz_sdk_types::{
    ControlCertificateRenewalAttempt, ControlCertificateRenewalOutcome, DeployReserveRequest,
    DeploySubmitRequest, MachineAddError, MachineAddRequest, MachineInspectRequest,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineListRequest, MachineTestimony,
    MachineUpdateError, MachineUpdateRequest, OpsWatchRequest, RuntimeDerivedCollectionStatus,
    RuntimePloyzDnsTargetAllocation, RuntimePloyzDnsTargetPublication, RuntimeSnapshot,
    RuntimeSnapshotRequest, ServiceInspectRequest, ServiceListRequest, VolumeListRequest,
    VolumeStatus,
};
use ployz_test_support::ops::wait_for_terminal_status;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::tests::support::control::{TestNats, machine_join_template, redeem_when_ready};
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    event_sequence, idempotency_key, machine_id, namespace_id, namespace_revision_entry_id,
    operation_id, route_hostname, route_port, service_id,
};

#[tokio::test]
async fn control_runtime_bootstraps_nats_and_serves_operation_api() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();

    let control_health = api
        .runtime_snapshot(&RuntimeSnapshotRequest {})
        .await
        .expect("runtime snapshot includes Control health")
        .control_health
        .expect("Control health is present");
    assert!(control_health.task_supervisor.active_tasks > 0);
    assert_eq!(control_health.task_supervisor.panicked_tasks, 0);
    assert_eq!(control_health.task_supervisor.last_failure, None);
    let renewal_health = control_health.certificate_renewal;
    assert_eq!(renewal_health.consecutive_failures, 0);
    assert!(matches!(
        renewal_health.last_attempt,
        None | Some(ControlCertificateRenewalAttempt::Completed {
            outcome: ControlCertificateRenewalOutcome::NoAction,
        })
    ));

    let request =
        reserved_deploy_request(&api, "idem_control_runtime", deploy_target("svc_api")).await;
    let accepted = api
        .deploy_submit(&request)
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
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
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
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
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

    // The minted per-machine seed is a working Machine credential: connect
    // with it and converge the target dataplane before reporting completion.
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
    let runner = ObservingContainerRunner::new(machine_id("machine_2"));
    let machine_runtime = start_machine_role_runtime(
        minted_client.clone(),
        machine_id("machine_2"),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id("machine_2")),
        runner,
    )
    .await
    .expect("machine runtime starts");
    join_api
        .machine_join_report(&MachineJoinReportRequest {
            join_token: accepted.join_token.clone(),
            outcome: MachineJoinReportOutcome::Completed,
        })
        .await
        .expect("join completion reports");
    wait_for_dataplane_projection(&nats, &machine_id("machine_2")).await;
    let status =
        wait_for_terminal_status(&api, &operation_id("op_machine"), Duration::from_secs(4)).await;
    assert!(
        matches!(
            status,
            OperationStatus::MachineAdd {
                state: ployz_core::operation::MachineAddOperationState::Completed,
                ..
            }
        ),
        "expected machine add to complete, got {status:?}"
    );
    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
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
            process_health: ployz_core::machine::GatewayProcessHealth::default(),
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
        crate::control::store::CoreStore::open(config.core_db_path.clone())
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
        .replace_volume_pin(VolumePinState::plain(
            namespace_id("default"),
            VolumeName::try_new("data").expect("valid volume name"),
            machine_id("core_1"),
        ))
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
    let config = nats.control_config().with_lease_worker_url(
        LeaseWorkerUrl::try_new("http://127.0.0.1:9").expect("lease worker URL"),
    );
    let namespace_intent = NamespaceIntentStore::new(
        crate::control::store::CoreStore::open(config.core_db_path.clone())
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
            id: RouteBindingId::try_new("route_api").expect("valid route binding id"),
            namespace_id: namespace_id("default"),
            target: RouteTarget::new(route_hostname("api.example.com")),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            origin: RouteBindingOrigin::Declared,
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
        snapshot.automatic_hostname_configuration,
        ployz_core::ingress::AutomaticHostnameConfiguration::Disabled
    );
    assert_eq!(
        snapshot.ployz_dns_target.allocation,
        RuntimePloyzDnsTargetAllocation::Unacquired
    );
    assert_eq!(
        snapshot.ployz_dns_target.publication,
        RuntimePloyzDnsTargetPublication::Unpublished
    );
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
async fn secured_operator_receives_passive_runtime_snapshot_replacements() {
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let config = nats.control_config();
    let machine_roster = machine_roster(&config).await;
    let mut snapshots = nats
        .connected
        .user
        .subscribe(RUNTIME_SNAPSHOT_STREAM)
        .await
        .expect("operator subscribes to runtime snapshots");
    nats.connected
        .user
        .flush()
        .await
        .expect("subscription flushes");
    let runtime = nats.start_control(&config).await;

    let initial = next_runtime_snapshot(&mut snapshots, "initial", |snapshot| {
        snapshot.machines.is_empty()
    })
    .await;
    assert!(initial.containers.is_empty());
    assert_eq!(
        initial.automatic_hostname_configuration,
        ployz_core::ingress::AutomaticHostnameConfiguration::Disabled
    );
    let seeded = request_json::<_, RuntimeSnapshot>(
        &nats.connected.user,
        RUNTIME_SNAPSHOT_SEED.to_owned(),
        &serde_json::json!({}),
        Duration::from_secs(2),
    )
    .await
    .expect("operator seeds from passive runtime projection");
    assert!(seeded.machines.is_empty());
    assert_eq!(
        seeded.automatic_hostname_configuration,
        initial.automatic_hostname_configuration
    );
    assert!(seeded.updated_at_unix_seconds >= initial.updated_at_unix_seconds);
    let health = nats
        .api()
        .runtime_snapshot(&RuntimeSnapshotRequest {})
        .await
        .expect("runtime snapshot includes Control health")
        .control_health
        .expect("Control health is present")
        .runtime_projection;
    assert_eq!(health.projection.consecutive_failures, 0);
    assert_eq!(health.publisher.consecutive_failures, 0);
    assert_eq!(health.seed_service.endpoint_tasks_started, 1);
    assert_eq!(health.seed_service.endpoint_tasks_finished, 0);

    machine_roster
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    nats.connected
        .controller
        .publish(INTENT_CHANGED, Vec::new().into())
        .await
        .expect("intent invalidation publishes");
    nats.connected
        .controller
        .flush()
        .await
        .expect("intent flushes");
    next_runtime_snapshot(&mut snapshots, "intent", |snapshot| {
        snapshot.machines.len() == 1
    })
    .await;

    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    publish_machine_facts(
        &machine_client,
        containers::snapshot(
            "machine_a",
            [containers::observation("machine_a", "ctr_passive").running_unroutable()],
        ),
        None,
    )
    .await;
    next_runtime_snapshot(&mut snapshots, "machine", |snapshot| {
        snapshot
            .containers
            .iter()
            .any(|container| container.container_id.as_str() == "ctr_passive")
    })
    .await;

    publish_gateway_status(
        &machine_client,
        GatewayStatusObservation {
            machine_id: machine_id("machine_a"),
            listen_addr: "127.0.0.1:443".parse().expect("gateway address"),
            serving: GatewayServingStatus::Current,
            route_count: 2,
            process_health: ployz_core::machine::GatewayProcessHealth::default(),
        },
    )
    .await;
    let gateway = next_runtime_snapshot(&mut snapshots, "gateway", |snapshot| {
        matches!(
            snapshot.machines.first().map(|machine| &machine.testimony),
            Some(MachineTestimony::Answered {
                gateway: Some(gateway),
                ..
            }) if gateway.route_count == 2
        )
    })
    .await;
    assert_eq!(gateway.machines.len(), 1);

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

async fn next_runtime_snapshot(
    snapshots: &mut async_nats::Subscriber,
    expected: &str,
    accept: impl Fn(&RuntimeSnapshot) -> bool,
) -> RuntimeSnapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = snapshots.next().await.expect("runtime stream stays open");
            let snapshot = serde_json::from_slice::<RuntimeSnapshot>(&message.payload)
                .expect("runtime snapshot decodes");
            if accept(&snapshot) {
                return snapshot;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{expected} runtime snapshot arrives"))
}

#[tokio::test]
async fn control_runtime_refuses_machine_add_without_join_template() {
    let nats = TestNats::start().await;
    let result = crate::control::process::start_control_process_with_client_and_reload(
        nats.connected.controller.clone(),
        &nats.control_config_without_join_template(),
        nats.reload_runner(),
    )
    .await;

    assert!(matches!(
        result,
        Err(crate::control::process::ControlProcessError::MissingMachineJoinTemplate)
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
    let machine_runtime = start_machine_role_runtime(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id("machine_a")),
        runner.clone(),
    )
    .await
    .expect("machine runtime starts");
    wait_for_dataplane_projection(&nats, &machine_id("machine_a")).await;
    let api = nats.api();
    let credential = RegistryCredential::try_basic("alice", "deploy-only-secret")
        .expect("valid registry credential");
    let mut request = reserved_deploy_request(&api, "idem_run", deploy_target("svc_api")).await;
    request.registry_credentials =
        std::collections::BTreeMap::from([(service_id("svc_api"), credential.clone())]);

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
    let replay = api
        .ops_watch(&OpsWatchRequest {
            operation_id: accepted.operation_id.clone(),
            start_sequence: event_sequence(1),
            limit: OperationEventReplayLimit::try_new(100).expect("valid replay limit"),
        })
        .await
        .expect("deploy evidence replays");
    let namespace_intent = NamespaceIntentStore::new(
        crate::control::store::CoreStore::open(config.core_db_path.clone())
            .await
            .expect("open core store"),
    );
    let digest = OciDigest::sha256(b"ghcr.io/acme/api:rev-2");
    let pinned = image("ghcr.io/acme/api:rev-2")
        .with_digest(&digest)
        .expect("resolved image pins to digest");
    let entry = namespace_intent
        .load()
        .await
        .expect("namespace intent reads")
        .serving_target_entries
        .into_iter()
        .find(|entry| {
            entry.namespace_id == namespace_id("default")
                && entry.service_id == service_id("svc_api")
        })
        .expect("serving target committed");
    assert_eq!(entry.image, pinned);
    assert_eq!(
        entry.namespace_revision_entry_id,
        ployz_core::deploy::namespace_revision_entry_id_for(
            &namespace_id("default"),
            &service_id("svc_api"),
            &entry.image,
            &ployz_core::deploy::ImageSource::Registry,
            &ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
        )
    );
    assert_eq!(
        runner.resolutions(),
        vec![(image("ghcr.io/acme/api:rev-2"), Some(credential.clone()))]
    );
    let pulls = runner.pulls();
    let [
        crate::roles::machine::protocol::MachineImagePull::Registry {
            reference,
            credential: pull_credential,
        },
    ] = pulls.as_slice()
    else {
        panic!("one registry pull was recorded")
    };
    assert_eq!(reference, &entry.image);
    assert_eq!(pull_credential.as_ref(), Some(&credential));
    assert!(replay.events.iter().any(|event| {
        matches!(
            &event.event,
            OperationEvent::DeployImageResolved {
                service_id,
                machine_id,
                requested,
                resolved,
                credential_supplied: true,
                ..
            } if service_id == &self::service_id("svc_api")
                && machine_id == &self::machine_id("machine_a")
                && requested == &self::image("ghcr.io/acme/api:rev-2")
                && resolved == &entry.image
        )
    }));
    assert!(
        !serde_json::to_string(&replay)
            .expect("deploy evidence serializes")
            .contains("deploy-only-secret"),
        "deploy-scoped registry secret reached operation evidence"
    );
    assert!(
        !std::fs::read(&config.core_db_path)
            .expect("core database reads")
            .windows(b"deploy-only-secret".len())
            .any(|window| window == b"deploy-only-secret"),
        "deploy-scoped registry secret reached durable core storage"
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
async fn control_runtime_records_typed_planning_failure_when_tag_cannot_resolve() {
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
    runner.fail_registry_resolution("registry denied the manifest");
    let machine_runtime = start_machine_role_runtime(
        machine_client,
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id("machine_a")),
        runner.clone(),
    )
    .await
    .expect("machine runtime starts");
    wait_for_dataplane_projection(&nats, &machine_id("machine_a")).await;
    let api = nats.api();
    let request =
        reserved_deploy_request(&api, "idem_resolution_failure", deploy_target("svc_api")).await;
    let accepted = api
        .deploy_submit(&request)
        .await
        .expect("deploy submits before planning");

    let status =
        wait_for_terminal_status(&api, &accepted.operation_id, Duration::from_secs(4)).await;
    let OperationStatus::Deploy {
        state:
            DeployOperationState::Failed {
                failure:
                    ployz_core::operation::DeployOperationFailure::ImageResolutionFailed {
                        service_id,
                        machine_id,
                        image,
                        message,
                    },
            },
        ..
    } = status
    else {
        panic!("expected typed image resolution failure, got {status:?}")
    };
    assert_eq!(service_id, self::service_id("svc_api"));
    assert_eq!(machine_id, self::machine_id("machine_a"));
    assert_eq!(image, self::image("ghcr.io/acme/api:rev-2"));
    assert_eq!(message.as_str(), "registry denied the manifest");
    assert!(
        runner.pulls().is_empty(),
        "planning failure started no container"
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
        state: ployz_core::operation::MachineUpdateOperationState::Completed { reported },
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
    let nats = TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    let config = nats
        .control_config()
        .with_deploy_machines(vec![machine_id("machine_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let machine_roster = machine_roster(&config).await;
    let runtime = nats
        .start_control_with_test_issuer(&config, std::sync::Arc::new(FixtureAcmeIssuer))
        .await;
    let machine_client = nats.machine_client(&machine_id("machine_a")).await;
    machine_roster
        .replace_active_machine(&active_machine("machine_a"))
        .await
        .expect("active machine stores");
    let runner = ObservingContainerRunner::new(machine_id("machine_a"));
    let machine_runtime = start_machine_role_runtime_with_endpoint_observation(
        machine_client.clone(),
        machine_id("machine_a"),
        runner.clone(),
        ReadyWireGuardEbpf::for_machine(&machine_id("machine_a")),
        runner.clone(),
        MachineEndpointObservation {
            machine_id: machine_id("machine_a"),
            control_endpoints: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            mesh_endpoints: Vec::new(),
        },
    )
    .await
    .expect("machine runtime starts");
    wait_for_dataplane_projection(&nats, &machine_id("machine_a")).await;
    let gateway = start_gateway_process_with_client(
        machine_client.clone(),
        Duration::from_millis(10),
        SocketAddr::from(([127, 0, 0, 1], 0)),
        machine_id("machine_a"),
        None,
    )
    .await
    .expect("gateway runtime starts");
    let upstream = crate::tests::support::TestHttpUpstream::start("smoke").await;
    let api = nats.api();

    let request = reserved_deploy_request(
        &api,
        "idem_routed",
        deploy_target_with_route("svc_api", upstream.port()),
    )
    .await;
    let accepted = api
        .deploy_submit(&request)
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
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("write gateway request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read gateway response");

    assert!(response.starts_with("HTTP/1.1 301 Moved Permanently\r\n"));
    assert!(response.contains("location: https://localhost/smoke\r\n"));

    gateway.shutdown().await.expect("gateway shuts down");
    machine_runtime
        .shutdown()
        .await
        .expect("machine runtime shuts down");
    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

struct FixtureAcmeIssuer;

#[async_trait::async_trait]
impl AcmeIssuer for FixtureAcmeIssuer {
    async fn issue_http01(
        &self,
        context: &AcmeIssueContext,
        hostname: &ployz_core::operation::RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        let challenge = ployz_core::certificate::AcmeHttp01Challenge::try_new(
            hostname.clone(),
            ployz_core::certificate::AcmeChallengeToken::try_new("control-runtime-token")
                .expect("challenge token"),
            ployz_core::certificate::AcmeChallengeValue::try_new(
                "control-runtime-token.fixture-thumbprint",
            )
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

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

async fn machine_roster(config: &crate::config::ControlProcessConfig) -> MachineRosterStore {
    MachineRosterStore::new(
        crate::control::store::CoreStore::open(config.core_db_path.clone())
            .await
            .expect("open core store"),
    )
}

async fn wait_for_dataplane_projection(nats: &TestNats, machine_id: &MachineId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let response = request_json::<_, MachineDataplaneStatusRpcResponse>(
            &nats.connected.controller,
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

fn active_machine(value: &str) -> ActiveMachineState {
    ActiveMachineState {
        control_endpoints: Vec::new(),
        mesh_endpoints: Vec::new(),
        lifecycle: MachineLifecycle::Active,
        machine_id: machine_id(value),
        name: ployz_sdk_types::MachineName::try_new(value).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
        roles: ployz_core::machine::roles::InstallRolePolicy::install_all(),
        endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.198.0.0/24")
            .expect("valid endpoint subnet"),
        wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(format!(
            "public-{value}"
        ))
        .expect("public key"),
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

fn test_disk_space() -> ployz_core::machine::runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
}

fn empty_machine_snapshot(value: &str) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(value), [])
        .expect("empty machine snapshot is valid")
}

async fn reserved_deploy_request(
    api: &OperationApiClient,
    idempotency: &str,
    target: DeployRequest,
) -> DeploySubmitRequest {
    let reservation = api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: target.namespace_id.clone(),
        })
        .await
        .expect("deploy reservation is issued");
    DeploySubmitRequest {
        registry_credentials: std::collections::BTreeMap::new(),
        idempotency_key: idempotency_key(idempotency),
        reservation_id: reservation.reservation_id,
        target,
    }
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

fn deploy_target_with_route(service_id: &str, endpoint_port: u16) -> DeployRequest {
    let mut target = deploy_target(service_id);
    let [service] = target.services.as_mut_slice() else {
        panic!("deploy target fixture has one service");
    };
    service.routes = vec![DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname: route_hostname("localhost"),
        },
        endpoint_port: route_port(endpoint_port),
    }];
    target
}
