//! Authenticated-connection contract against a real TLS + NKey-authorized
//! `nats-server`: valid credentials connect, invalid credentials are
//! rejected, subject permissions are enforced, and per-principal inbox
//! prefixes make reply sniffing impossible.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::build::{
    BuildAdapter, BuildContextPath, BuildExecutorAssignment, BuildExecutorCancelRequest,
    BuildExecutorStartRequest, GitSource,
};
use ployz_core::ids::{BuildExecutorId, BuildPoolId, MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    MintedNatsUser,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsConnectConfig, authenticated_connect_options, connect_authenticated,
};
use ployz_nats::permissions::{inbox_prefix, inbox_subscribe_scope};
use ployz_nats::service_runtime::{NatsServiceResponse, start_nats_service};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata, ServiceVersion,
};
use ployz_nats::subjects::{
    BUILD_EXECUTOR_SERVICE_NAME, BuildExecutorServiceEndpoint, CoreQueryEndpoint, INTENT_CHANGED,
    INTENT_GET, JOIN_MACHINE_REDEEM, JOIN_MACHINE_REPORT, MachineServiceEndpoint,
    OPERATOR_INIT_FIRST_MACHINE_ACTIVATE, PENDING_MACHINE_JOINS_CHANGED, RUNTIME_SNAPSHOT_SEED,
    RUNTIME_SNAPSHOT_STREAM, build_executor_log, build_executor_service, gateway_status,
    gateway_status_scope, machine_container_facts, machine_facts, machine_facts_scope,
    machine_service,
};
use ployz_test_support::nats::SecuredTestNats;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const NO_DELIVERY_WINDOW: Duration = Duration::from_millis(500);

fn external_executor_credential(
    minted: &MintedNatsUser,
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> CredentialGrant {
    CredentialGrant {
        public_key: minted.public.clone(),
        name: CredentialName::try_new("Test external Build Executor").expect("credential name"),
        role: CredentialRole::BuildExecutor {
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
            expires_at: BuildExecutorCredentialExpiresAt::try_new(2_000_000_000)
                .expect("future expiry"),
        },
    }
}

fn external_executor_principal(
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> NatsPrincipal {
    NatsPrincipal::BuildExecutor {
        pool_id: pool_id.clone(),
        executor_id: executor_id.clone(),
    }
}

#[tokio::test]
async fn valid_seed_and_ca_connects_over_tls() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");

    let client = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller seed + cluster CA connects");

    assert!(
        client.server_info().tls_required,
        "fixture server must require TLS"
    );
    client
        .flush()
        .await
        .expect("authenticated client round-trips");
}

#[tokio::test]
async fn wrong_seed_is_rejected() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let unauthorized_seed =
        SecuredTestNats::fresh_unauthorized_seed().expect("fresh unauthorized seed");
    let config = fixture.config_with_seed(NatsPrincipal::Join, unauthorized_seed);

    let result = connect_authenticated(&config, CONNECT_TIMEOUT).await;

    assert!(
        result.is_err(),
        "a seed outside the authorized user set must not connect"
    );
}

#[tokio::test]
async fn extra_cloud_operator_public_key_can_connect_as_operator() {
    let cloud_user = MintedNatsUser::generate().expect("cloud nkey mints");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let fixture = SecuredTestNats::start_with_machines_and_extra_users(
        std::slice::from_ref(&machine_id),
        std::slice::from_ref(&cloud_user.public),
    )
    .await
    .expect("secured fixture");

    let (client, mut events) = connect_with_event_capture(
        &fixture.config_with_seed(NatsPrincipal::Operator, cloud_user.seed),
    )
    .await;

    client
        .publish(INTENT_GET, "cloud intent".into())
        .await
        .expect("cloud publishes current Operator endpoint");
    for endpoint in MachineServiceEndpoint::IMAGE_TRANSFER.iter().copied() {
        client
            .publish(machine_service(&machine_id, endpoint), "image".into())
            .await
            .expect("cloud-as-Operator publishes image transfer endpoint");
    }
    client.flush().await.expect("cloud operator round-trips");
    assert_no_permission_violation(&mut events).await;
    client
        .publish(
            machine_service(&machine_id, MachineServiceEndpoint::ContainerRun),
            "non-image".into(),
        )
        .await
        .expect("non-image publish accepted client-side");
    client.flush().await.expect("non-image publish flushes");
    assert_permission_violation_kind(&mut events, "Publish").await;
}

#[tokio::test]
async fn controller_service_registration_needs_no_inbox_publish_authority() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let (controller, mut events) = connect_with_event_capture(&fixture.controller_config()).await;
    let endpoint = NatsServiceEndpointSpec::new(
        "init.first_machine.activate",
        OPERATOR_INIT_FIRST_MACHINE_ACTIVATE,
        EndpointExecution::MutatesOperation,
    );
    let spec = NatsServiceSpec::new(
        "registration-proof",
        "plz-api",
        ServiceVersion::new(0, 1, 0),
        "registration proof",
        ServiceMetadata::empty(),
        vec![endpoint.clone()],
    );
    let mut service = start_nats_service(controller, &spec)
        .await
        .expect("controller starts service with finite discovery authority");
    service
        .bind_endpoint(&endpoint, |_request| async {
            NatsServiceResponse::ok(b"activated".to_vec())
        })
        .await
        .expect("flush barrier proves endpoint registration");
    assert_no_permission_violation(&mut events).await;
    service.shutdown().await.expect("service shuts down");
}

#[tokio::test]
async fn external_build_executor_serves_its_endpoints_logs_and_image_requests() {
    let pool_id = BuildPoolId::try_new("pool-a").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor-a").expect("executor id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let operation_id = OperationId::try_new("build-7").expect("operation id");
    let minted = MintedNatsUser::generate().expect("executor nkey mints");
    let credential = external_executor_credential(&minted, &pool_id, &executor_id);
    let fixture = SecuredTestNats::start_with_machines_and_credentials(
        std::slice::from_ref(&machine_id),
        std::slice::from_ref(&credential),
    )
    .await
    .expect("secured fixture");
    let principal = external_executor_principal(&pool_id, &executor_id);
    let (executor, mut executor_events) =
        connect_with_event_capture(&fixture.config_with_seed(principal, minted.seed)).await;
    let controller = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller connects");
    let machine_config = fixture
        .machine_config(&machine_id)
        .expect("fixture minted machine");
    let machine = connect_authenticated(&machine_config, CONNECT_TIMEOUT)
        .await
        .expect("machine connects");

    let readiness_subject = build_executor_service(
        &pool_id,
        &executor_id,
        BuildExecutorServiceEndpoint::ReadinessGet,
    );
    let start_subject = build_executor_service(
        &pool_id,
        &executor_id,
        BuildExecutorServiceEndpoint::BuildStart,
    );
    let cancel_subject = build_executor_service(
        &pool_id,
        &executor_id,
        BuildExecutorServiceEndpoint::BuildCancel,
    );
    let instance = "external-executor-a";
    let discovery_subjects = ["PING", "INFO", "STATS"]
        .into_iter()
        .flat_map(|verb| {
            [
                format!("$SRV.{verb}"),
                format!("$SRV.{verb}.{BUILD_EXECUTOR_SERVICE_NAME}"),
                format!("$SRV.{verb}.{BUILD_EXECUTOR_SERVICE_NAME}.{instance}"),
            ]
        })
        .collect::<Vec<_>>();
    let mut responder_tasks = Vec::new();
    for subject in [start_subject.clone(), cancel_subject.clone()]
        .into_iter()
        .chain(discovery_subjects.iter().cloned())
        .chain(std::iter::once(readiness_subject.clone()))
    {
        let mut requests = executor
            .subscribe(subject)
            .await
            .expect("executor subscribes exact granted service subject");
        let responder = executor.clone();
        responder_tasks.push(tokio::spawn(async move {
            while let Some(request) = requests.next().await {
                let Some(reply) = request.reply else {
                    continue;
                };
                responder.publish(reply, request.payload).await.ok();
                responder.flush().await.ok();
            }
        }));
    }

    // The known-set readiness subject is the registration proof. The bounded
    // request retries only while the exact subscription reaches nats-server.
    let readiness = request_when_responder_ready(&controller, &readiness_subject, "ready").await;
    assert_eq!(readiness.payload.as_ref(), b"ready");

    let assignment = BuildExecutorAssignment::External {
        pool_id: pool_id.clone(),
        executor_id: executor_id.clone(),
        image_seed: machine_id.clone(),
    };
    let start_request = BuildExecutorStartRequest {
        operation_id: operation_id.clone(),
        assignment: assignment.clone(),
        source: GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "redacted-test-value",
            None::<String>,
        )
        .expect("source"),
        adapter: BuildAdapter::Dockerfile {
            dockerfile: BuildContextPath::try_new("Dockerfile").expect("dockerfile path"),
            target: None,
        },
        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        timeout_millis: 1_000,
    };
    let start_payload = serde_json::to_vec(&start_request).expect("start request encodes");
    let start_response = controller
        .request(start_subject, start_payload.clone().into())
        .await
        .expect("controller requests build start");
    assert_eq!(start_response.payload.as_ref(), start_payload);

    let cancel_request = BuildExecutorCancelRequest {
        operation_id: operation_id.clone(),
        assignment,
    };
    let cancel_payload = serde_json::to_vec(&cancel_request).expect("cancel request encodes");
    let cancel_response = controller
        .request(cancel_subject, cancel_payload.clone().into())
        .await
        .expect("controller requests build cancel");
    assert_eq!(cancel_response.payload.as_ref(), cancel_payload);

    let mut logs = controller
        .subscribe(build_executor_log(&pool_id, &executor_id, &operation_id))
        .await
        .expect("controller subscribes executor log");
    controller.flush().await.expect("log subscription flushes");
    executor
        .publish(
            build_executor_log(&pool_id, &executor_id, &operation_id),
            "build output".into(),
        )
        .await
        .expect("executor publishes own log");
    let log = tokio::time::timeout(EVENT_TIMEOUT, logs.next())
        .await
        .expect("log arrives before timeout")
        .expect("log subscription remains open");
    assert_eq!(log.payload.as_ref(), b"build output");

    for endpoint in [
        MachineServiceEndpoint::ImageBlobCheck,
        MachineServiceEndpoint::ImageBlobPush,
        MachineServiceEndpoint::ImageManifestPush,
    ] {
        let subject = machine_service(&machine_id, endpoint);
        let mut requests = machine
            .subscribe(subject.clone())
            .await
            .expect("machine subscribes image endpoint");
        machine.flush().await.expect("image endpoint flushes");
        let responder = machine.clone();
        tokio::spawn(async move {
            if let Some(request) = requests.next().await
                && let Some(reply) = request.reply
            {
                responder.publish(reply, "image-ok".into()).await.ok();
                responder.flush().await.ok();
            }
        });
        let response = request_when_responder_ready(&executor, &subject, "image-request").await;
        assert_eq!(response.payload.as_ref(), b"image-ok");
    }

    assert_no_permission_violation(&mut executor_events).await;
    for task in responder_tasks {
        task.abort();
    }
}

#[tokio::test]
async fn external_build_executor_is_denied_other_authority_by_the_server() {
    let pool_id = BuildPoolId::try_new("pool-a").expect("pool id");
    let other_pool_id = BuildPoolId::try_new("pool-b").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor-a").expect("executor id");
    let other_executor_id = BuildExecutorId::try_new("executor-b").expect("executor id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let operation_id = OperationId::try_new("build-7").expect("operation id");
    let minted = MintedNatsUser::generate().expect("executor nkey mints");
    let credential = external_executor_credential(&minted, &pool_id, &executor_id);
    let fixture = SecuredTestNats::start_with_machines_and_credentials(
        std::slice::from_ref(&machine_id),
        std::slice::from_ref(&credential),
    )
    .await
    .expect("secured fixture");
    let principal = external_executor_principal(&pool_id, &executor_id);
    let (executor, mut events) =
        connect_with_event_capture(&fixture.config_with_seed(principal, minted.seed)).await;

    for denied in [
        build_executor_service(
            &pool_id,
            &other_executor_id,
            BuildExecutorServiceEndpoint::BuildStart,
        ),
        build_executor_service(
            &other_pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::ReadinessGet,
        ),
        machine_service(&machine_id, MachineServiceEndpoint::BuildStart),
        "$SRV.PING.some-other-service".to_owned(),
        format!("$SRV.PING.{BUILD_EXECUTOR_SERVICE_NAME}.instance.extra"),
        "plz.v1.rpc.build_executor.command.pool-a.executor-a.future.run".to_owned(),
    ] {
        let _subscription = executor
            .subscribe(denied)
            .await
            .expect("subscribe call is accepted client-side");
        executor.flush().await.expect("denied subscription flushes");
        assert_permission_violation_kind(&mut events, "Subscription").await;
    }

    for (denied, payload) in [
        (
            build_executor_log(&other_pool_id, &executor_id, &operation_id),
            "cross-pool-log",
        ),
        (
            build_executor_log(&pool_id, &other_executor_id, &operation_id),
            "cross-executor-log",
        ),
        (
            OPERATOR_INIT_FIRST_MACHINE_ACTIVATE.to_owned(),
            "operator-rpc",
        ),
        (
            machine_service(&machine_id, MachineServiceEndpoint::ContainerRun),
            "container-run",
        ),
        (
            machine_service(&machine_id, MachineServiceEndpoint::ImageEnsure),
            "image-ensure",
        ),
        (
            machine_service(&machine_id, MachineServiceEndpoint::ImageRemove),
            "image-remove",
        ),
        (
            "plz.v1.rpc.machine.command.machine-a.image.layer.push".to_owned(),
            "future-image-transfer",
        ),
        (machine_facts(&machine_id), "machine-facts"),
        (INTENT_CHANGED.to_owned(), "intent-changed"),
        (
            PENDING_MACHINE_JOINS_CHANGED.to_owned(),
            "membership-changed",
        ),
    ] {
        executor
            .publish(denied, payload.into())
            .await
            .expect("publish call is accepted client-side");
        executor.flush().await.expect("denied publish flushes");
        assert_permission_violation_kind(&mut events, "Publish").await;
    }
}

#[tokio::test]
async fn external_build_executor_cannot_self_address_a_request_to_gain_reply_authority() {
    let pool_id = BuildPoolId::try_new("pool-a").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor-a").expect("executor id");
    let minted = MintedNatsUser::generate().expect("executor nkey mints");
    let credential = external_executor_credential(&minted, &pool_id, &executor_id);
    let fixture = SecuredTestNats::start_with_machines_and_credentials(
        &[],
        std::slice::from_ref(&credential),
    )
    .await
    .expect("secured fixture");
    let principal = external_executor_principal(&pool_id, &executor_id);
    let own_inbox = format!("{}.escalation", inbox_prefix(&principal));
    let (executor, mut events) =
        connect_with_event_capture(&fixture.config_with_seed(principal, minted.seed)).await;
    let controller = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller connects");
    let mut self_requests = executor
        .subscribe(own_inbox.clone())
        .await
        .expect("executor subscribes its own inbox");
    let mut forbidden = controller
        .subscribe(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE)
        .await
        .expect("controller observes forbidden Operator subject");
    executor
        .flush()
        .await
        .expect("executor subscription flushes");
    controller
        .flush()
        .await
        .expect("observer subscription flushes");

    executor
        .publish_with_reply(
            own_inbox,
            OPERATOR_INIT_FIRST_MACHINE_ACTIVATE.to_owned(),
            "malicious request".into(),
        )
        .await
        .expect("publish is accepted client-side");
    executor.flush().await.expect("denied publish flushes");

    assert_permission_violation_kind(&mut events, "Publish").await;
    assert!(
        tokio::time::timeout(NO_DELIVERY_WINDOW, self_requests.next())
            .await
            .is_err(),
        "the denied self-addressed request must not reach the executor"
    );
    assert!(
        tokio::time::timeout(NO_DELIVERY_WINDOW, forbidden.next())
            .await
            .is_err(),
        "self-addressing must not mint reply authority for a forbidden subject"
    );
}

#[tokio::test]
async fn build_executor_grant_preserves_operator_and_machine_traffic() {
    let pool_id = BuildPoolId::try_new("pool-a").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor-a").expect("executor id");
    let machine_id = MachineId::try_new("machine-a").expect("machine id");
    let minted = MintedNatsUser::generate().expect("executor nkey mints");
    let credential = external_executor_credential(&minted, &pool_id, &executor_id);
    let fixture = SecuredTestNats::start_with_machines_and_credentials(
        std::slice::from_ref(&machine_id),
        std::slice::from_ref(&credential),
    )
    .await
    .expect("secured fixture");
    let controller = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller connects");
    let operator = connect_authenticated(&fixture.user_config(), CONNECT_TIMEOUT)
        .await
        .expect("operator connects");
    let machine_config = fixture
        .machine_config(&machine_id)
        .expect("fixture minted machine");
    let machine = connect_authenticated(&machine_config, CONNECT_TIMEOUT)
        .await
        .expect("machine connects");

    let mut intent_requests = controller
        .subscribe(INTENT_GET)
        .await
        .expect("controller subscribes intent endpoint");
    let mut facts = controller
        .subscribe(machine_facts(&machine_id))
        .await
        .expect("controller subscribes machine facts");
    controller
        .flush()
        .await
        .expect("controller subscriptions flush");
    let responder = controller.clone();
    tokio::spawn(async move {
        if let Some(request) = intent_requests.next().await
            && let Some(reply) = request.reply
        {
            responder.publish(reply, "intent".into()).await.ok();
            responder.flush().await.ok();
        }
    });

    let intent = request_when_responder_ready(&operator, INTENT_GET, "get").await;
    assert_eq!(intent.payload.as_ref(), b"intent");
    machine
        .publish(machine_facts(&machine_id), "facts".into())
        .await
        .expect("machine publishes facts");
    let fact = tokio::time::timeout(EVENT_TIMEOUT, facts.next())
        .await
        .expect("facts arrive before timeout")
        .expect("facts subscription remains open");
    assert_eq!(fact.payload.as_ref(), b"facts");
}

#[tokio::test]
async fn operator_can_read_runtime_and_publish_only_image_transfer_machine_commands() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let (operator, mut events) = connect_with_event_capture(&fixture.user_config()).await;
    let machine_id = MachineId::try_new("machine-a").expect("machine id");

    let _runtime_snapshots = operator
        .subscribe(RUNTIME_SNAPSHOT_STREAM)
        .await
        .expect("operator subscribes runtime projection");
    operator
        .publish(INTENT_GET, "read intent".into())
        .await
        .expect("operator publishes intent read request");
    operator
        .publish(RUNTIME_SNAPSHOT_SEED, "{}".into())
        .await
        .expect("operator publishes runtime seed request");
    for endpoint in MachineServiceEndpoint::IMAGE_TRANSFER.iter().copied() {
        operator
            .publish(machine_service(&machine_id, endpoint), "image".into())
            .await
            .expect("Operator publishes image transfer endpoint");
    }
    operator.flush().await.expect("flush allowed subjects");
    assert_no_permission_violation(&mut events).await;

    for denied in [
        machine_facts_scope(),
        gateway_status_scope(),
        INTENT_CHANGED.to_owned(),
        PENDING_MACHINE_JOINS_CHANGED.to_owned(),
    ] {
        let _denied = operator
            .subscribe(denied)
            .await
            .expect("subscribe call is accepted client-side");
        operator.flush().await.expect("flush denied subject");
        let violation = next_permission_violation(&mut events).await;
        assert!(
            violation.contains("Subscription"),
            "expected a subscription violation, got: {violation}"
        );
    }
    for denied in [
        "plz.v1.rpc.operator.query.future.read".to_owned(),
        "plz.v1.rpc.operator.command.future.write".to_owned(),
        "plz.v1.rpc.machine.command.machine-a.image.layer.push".to_owned(),
        machine_service(&machine_id, MachineServiceEndpoint::ContainerRun),
    ] {
        operator
            .publish(denied, "future".into())
            .await
            .expect("future publish accepted client-side");
        operator.flush().await.expect("future publish flushes");
        assert_permission_violation_kind(&mut events, "Publish").await;
    }
}

#[tokio::test]
async fn plaintext_connect_to_tls_port_fails() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let address = SocketAddr::from(([127, 0, 0, 1], fixture.port()));
    let mut plaintext = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
        .expect("plaintext TCP reaches the listener");
    plaintext
        .set_read_timeout(Some(NO_DELIVERY_WINDOW))
        .expect("plaintext read timeout configures");
    plaintext
        .set_write_timeout(Some(NO_DELIVERY_WINDOW))
        .expect("plaintext write timeout configures");

    let mut info_line = String::new();
    BufReader::new(&plaintext)
        .read_line(&mut info_line)
        .expect("NATS listener sends its INFO prelude");
    let info = info_line
        .strip_prefix("INFO ")
        .expect("NATS prelude starts with INFO");
    let info: serde_json::Value =
        serde_json::from_str(info.trim()).expect("NATS INFO payload is JSON");
    assert_eq!(
        info.get("tls_required").and_then(|value| value.as_bool()),
        Some(true)
    );

    plaintext
        .write_all(b"CONNECT {\"verbose\":false}\r\nPING\r\n")
        .expect("plaintext NATS handshake writes");
    let mut response = [0; 4];
    let read = plaintext.read_exact(&mut response);
    assert!(
        read.is_err() || response != *b"PONG",
        "TLS-required listener accepted a plaintext NATS session"
    );
}

#[tokio::test]
async fn controller_cannot_publish_to_its_own_inbox() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let (controller, mut events) = connect_with_event_capture(&fixture.controller_config()).await;

    controller
        .publish(
            format!("{}.test", inbox_prefix(&NatsPrincipal::Controller)),
            "service reply".into(),
        )
        .await
        .expect("publish accepted client-side");
    controller.flush().await.expect("flush");

    assert_permission_violation_kind(&mut events, "Publish").await;
}

#[tokio::test]
async fn machine_publish_outside_allow_list_gets_permission_violation() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };

    let (client, mut events) = connect_with_event_capture(&config).await;
    client
        .publish(
            "plz.v1.obs.machine.other-machine.started",
            "evidence".into(),
        )
        .await
        .expect("publish is accepted client-side");
    client.flush().await.expect("flush");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Publish"),
        "expected a publish violation, got: {violation}"
    );
}

#[tokio::test]
async fn machine_cannot_publish_to_its_own_inbox() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let (machine, mut events) = connect_with_event_capture(&config).await;

    machine
        .publish(
            format!(
                "{}.test",
                inbox_prefix(&NatsPrincipal::Machine {
                    machine_id: machine_id.clone(),
                })
            ),
            "service reply".into(),
        )
        .await
        .expect("publish accepted client-side");
    machine.flush().await.expect("flush");

    assert_permission_violation_kind(&mut events, "Publish").await;
}

#[tokio::test]
async fn machine_fact_writes_are_fenced_to_its_own_subjects() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let other_machine_id = MachineId::try_new("machine-b").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let (machine_client, mut events) = connect_with_event_capture(&config).await;

    machine_client
        .publish(machine_facts(&machine_id), "facts".into())
        .await
        .expect("machine publishes its own facts");
    machine_client
        .publish(machine_container_facts(&machine_id), "delta".into())
        .await
        .expect("machine publishes its own fact delta");
    machine_client
        .publish(gateway_status(&machine_id), "status".into())
        .await
        .expect("machine publishes its own gateway status");
    machine_client.flush().await.expect("flush");
    assert_no_permission_violation(&mut events).await;

    machine_client
        .publish(machine_container_facts(&other_machine_id), "delta".into())
        .await
        .expect("publish is accepted client-side");
    machine_client.flush().await.expect("flush");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Publish"),
        "expected a publish violation, got: {violation}"
    );
}

#[tokio::test]
async fn machine_can_publish_every_core_query_but_not_a_future_query() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let config = fixture
        .machine_config(&machine_id)
        .expect("fixture mints requested machine");
    let (machine, mut events) = connect_with_event_capture(&config).await;

    for endpoint in CoreQueryEndpoint::ALL.iter().copied() {
        machine
            .publish(endpoint.subject(), "query".into())
            .await
            .expect("machine publishes current Core query endpoint");
    }
    machine.flush().await.expect("allowed Core queries flush");
    assert_no_permission_violation(&mut events).await;

    machine
        .publish("plz.v1.rpc.core.query.future.get", "future".into())
        .await
        .expect("future Core query publish accepted client-side");
    machine.flush().await.expect("future Core query flushes");
    assert_permission_violation_kind(&mut events, "Publish").await;
}

#[tokio::test]
async fn controller_can_serve_operator_rpc_subjects() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let server_config = fixture.controller_config();
    let caller_config = fixture.user_config();
    let server_client = connect_authenticated(&server_config, CONNECT_TIMEOUT)
        .await
        .expect("controller service connects");
    let caller_client = connect_authenticated(&caller_config, CONNECT_TIMEOUT)
        .await
        .expect("operator caller connects");
    let mut requests = server_client
        .subscribe(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE)
        .await
        .expect("controller subscribes API endpoint subject");
    server_client.flush().await.expect("flush");
    let responder = server_client.clone();
    tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            responder.publish(reply, "activated".into()).await.ok();
            responder.flush().await.ok();
        }
    });

    let response = request_when_responder_ready(
        &caller_client,
        OPERATOR_INIT_FIRST_MACHINE_ACTIVATE,
        "activate",
    )
    .await;

    assert_eq!(response.payload.as_ref(), b"activated");
}

#[tokio::test]
async fn controller_can_request_first_machine_activation() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let controller_config = fixture.controller_config();
    let service_client = connect_authenticated(&controller_config, CONNECT_TIMEOUT)
        .await
        .expect("controller service connects");
    let (caller_client, mut events) = connect_with_event_capture(&controller_config).await;
    let mut requests = service_client
        .subscribe(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE)
        .await
        .expect("controller subscribes activation endpoint");
    service_client.flush().await.expect("flush");
    let responder = service_client.clone();
    tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            responder.publish(reply, "activated".into()).await.ok();
            responder.flush().await.ok();
        }
    });

    let response = request_when_responder_ready(
        &caller_client,
        OPERATOR_INIT_FIRST_MACHINE_ACTIVATE,
        "activate",
    )
    .await;

    assert_eq!(response.payload.as_ref(), b"activated");
    assert_no_permission_violation(&mut events).await;
    for denied in [
        "plz.v1.rpc.operator.command.future.internal",
        "plz.v1.rpc.core.query.future.get",
        "plz.v1.rpc.machine.command.machine-a.future.run",
        "plz.v1.rpc.build_executor.command.pool-a.executor-a.future.run",
    ] {
        caller_client
            .publish(denied, "future".into())
            .await
            .expect("future publish accepted client-side");
        caller_client.flush().await.expect("future publish flushes");
        assert_permission_violation_kind(&mut events, "Publish").await;
    }
    let _unrelated_discovery = caller_client
        .subscribe("$SRV.PING.unrelated-service.runtime")
        .await
        .expect("unrelated discovery accepted client-side");
    caller_client
        .flush()
        .await
        .expect("unrelated discovery flushes");
    assert_permission_violation_kind(&mut events, "Subscription").await;
}

async fn request_when_responder_ready(
    client: &async_nats::Client,
    subject: &str,
    payload: &'static str,
) -> async_nats::Message {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            match client.request(subject.to_owned(), payload.into()).await {
                Ok(response) => return response,
                Err(error) if error.kind() == async_nats::RequestErrorKind::NoResponders => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("responder did not become ready: {error}"),
            }
        }
    })
    .await
    .expect("responder becomes ready before the event timeout")
}

#[tokio::test]
async fn machine_can_serve_machine_rpc_and_service_discovery_subjects() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let (machine_client, mut events) = connect_with_event_capture(&config).await;

    let mut endpoint_subscriptions = Vec::new();
    for endpoint in MachineServiceEndpoint::ALL.iter().copied() {
        endpoint_subscriptions.push(
            machine_client
                .subscribe(machine_service(&machine_id, endpoint))
                .await
                .expect("machine subscribes exact endpoint"),
        );
    }
    let _service_ping = machine_client
        .subscribe("$SRV.PING")
        .await
        .expect("machine subscribes service ping discovery");
    let _service_info = machine_client
        .subscribe("$SRV.INFO.plz-machine")
        .await
        .expect("machine subscribes service info discovery");
    machine_client.flush().await.expect("flush");

    assert_no_permission_violation(&mut events).await;
    for denied in [
        "plz.v1.rpc.machine.command.machine-a.future.run".to_owned(),
        machine_service(
            &MachineId::try_new("machine-b").expect("machine id"),
            MachineServiceEndpoint::Inspect,
        ),
        "$SRV.PING.plz-machine.runtime.extra".to_owned(),
        "$SRV.PING.plz-api.runtime".to_owned(),
    ] {
        let _denied = machine_client
            .subscribe(denied)
            .await
            .expect("denied subscribe accepted client-side");
        machine_client
            .flush()
            .await
            .expect("denied subscribe flushes");
        assert_permission_violation_kind(&mut events, "Subscription").await;
    }
}

#[tokio::test]
async fn join_can_publish_join_endpoints_but_not_general_operation_api_subjects() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let (join_client, mut events) = connect_with_event_capture(&fixture.join_config()).await;
    for subject in [JOIN_MACHINE_REDEEM, JOIN_MACHINE_REPORT] {
        join_client
            .publish(subject, "join".into())
            .await
            .expect("Join publishes current join endpoint");
    }
    join_client.flush().await.expect("allowed joins flush");
    assert_no_permission_violation(&mut events).await;

    join_client
        .publish(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE, "activate".into())
        .await
        .expect("publish is accepted client-side");
    join_client.flush().await.expect("flush");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Publish"),
        "expected a publish violation, got: {violation}"
    );
}

#[tokio::test]
async fn join_cannot_sniff_other_principals_inboxes() {
    let machine_id = MachineId::try_new("machine-x").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");

    // Join sniffer: subscribe the shared legacy inbox scope and other
    // principals' prefixes. The server must reject every one of them.
    let join_config = fixture.join_config();
    let (join_client, mut join_events) = connect_with_event_capture(&join_config).await;
    let machine_principal = NatsPrincipal::Machine {
        machine_id: machine_id.clone(),
    };
    let controller_scope = inbox_subscribe_scope(&NatsPrincipal::Controller);
    let machine_scope = inbox_subscribe_scope(&machine_principal);
    let mut sniff_shared = join_client
        .subscribe("_INBOX.>")
        .await
        .expect("subscribe call is accepted client-side");
    let mut sniff_controller = join_client
        .subscribe(controller_scope.clone())
        .await
        .expect("subscribe call is accepted client-side");
    let mut sniff_machine = join_client
        .subscribe(machine_scope.clone())
        .await
        .expect("subscribe call is accepted client-side");
    join_client.flush().await.expect("flush");

    for _ in [
        "_INBOX.>",
        controller_scope.as_str(),
        machine_scope.as_str(),
    ] {
        let violation = next_permission_violation(&mut join_events).await;
        assert!(
            violation.contains("Subscription"),
            "expected a subscription violation, got: {violation}"
        );
    }

    // Machine responder for the controller's request.
    let Some(machine_config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let machine_client = connect_authenticated(&machine_config, CONNECT_TIMEOUT)
        .await
        .expect("machine connects");
    let request_subject = machine_service(&machine_id, MachineServiceEndpoint::Inspect);
    let mut requests = machine_client
        .subscribe(request_subject.clone())
        .await
        .expect("machine subscribes its service scope");
    machine_client.flush().await.expect("flush");
    let responder = machine_client.clone();
    tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            responder.publish(reply, "pong".into()).await.ok();
            responder.flush().await.ok();
        }
    });

    // Drive a Controller request-reply while the sniffer is subscribed.
    let controller_client = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller connects");
    let response = tokio::time::timeout(
        EVENT_TIMEOUT,
        controller_client.request(request_subject, "ping".into()),
    )
    .await
    .expect("request does not hang")
    .expect("controller receives the machine's reply");
    assert_eq!(response.payload.as_ref(), b"pong");
    assert!(
        response
            .subject
            .as_str()
            .starts_with(&format!("{}.", inbox_prefix(&NatsPrincipal::Controller))),
        "reply must arrive on the controller's own inbox prefix, got {}",
        response.subject.as_str()
    );

    // The Join sniffer observably received none of that traffic.
    let observed = tokio::time::timeout(NO_DELIVERY_WINDOW, async {
        tokio::select! {
            message = sniff_shared.next() => ("_INBOX.>", message),
            message = sniff_controller.next() => (controller_scope.as_str(), message),
            message = sniff_machine.next() => (machine_scope.as_str(), message),
        }
    })
    .await;
    match observed {
        Err(_) => {}
        Ok((scope, Some(message))) => panic!(
            "join sniffer on {scope} received request-reply traffic on {}",
            message.subject
        ),
        Ok((scope, None)) => {
            panic!("join sniffer on {scope} closed before the observation window elapsed")
        }
    }
}

/// Connects with the exact product option set plus an event capture channel
/// so tests can observe server-side permission violations.
async fn connect_with_event_capture(
    config: &NatsConnectConfig,
) -> (
    async_nats::Client,
    tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) {
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = authenticated_connect_options(config)
        .event_callback(move |event| {
            let events_tx = events_tx.clone();
            async move {
                events_tx.send(event).ok();
            }
        })
        .connect(config.url.as_str())
        .await
        .expect("authenticated connect with event capture");
    (client, events_rx)
}

async fn next_permission_violation(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) -> String {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let Some(event) = events.recv().await else {
                panic!("event channel closed before a permission violation arrived");
            };
            if let async_nats::Event::ServerError(async_nats::ServerError::Other(message)) = event
                && message
                    .to_ascii_lowercase()
                    .contains("permissions violation")
            {
                return message;
            }
        }
    })
    .await
    .expect("server reports a permission violation")
}

async fn assert_permission_violation_kind(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
    expected_kind: &str,
) {
    let violation = next_permission_violation(events).await;
    assert!(
        violation.contains(expected_kind),
        "expected a {expected_kind} violation, got: {violation}"
    );
}

async fn assert_no_permission_violation(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) {
    let observed = tokio::time::timeout(NO_DELIVERY_WINDOW, async {
        loop {
            let event = events.recv().await?;
            if let async_nats::Event::ServerError(async_nats::ServerError::Other(message)) = event
                && message
                    .to_ascii_lowercase()
                    .contains("permissions violation")
            {
                return Some(message);
            }
        }
    })
    .await;

    match observed {
        Err(_) | Ok(None) => {}
        Ok(Some(message)) => panic!("unexpected permission violation: {message}"),
    }
}
