use super::*;
use ployz_core::cert::{
    AutoLeaseState, LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt,
    ManagedCertificateIssuanceFailureKind, ManagedLeaseAcquireRequest, ManagedLeaseIntent,
    ManagedLeaseName, ManagedLeaseRecord,
};
use ployz_core::ops::{
    EventSequence, OperationEvent, OperationEventReplayLimit, OperationEventReplayRequest,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn auto_hostname_deploy_waits_for_managed_certificate_and_stores_ready_bundle() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind lease worker");
    let address = listener.local_addr().expect("lease worker address");
    let worker = tokio::spawn(ployz_lease_worker::serve(
        listener,
        Arc::new(tokio::sync::Mutex::new(
            ployz_lease_worker::StubLeaseWorker::new(),
        )),
    ));
    let lease_client = LeaseClient::new(
        LeaseWorkerUrl::try_new(format!("http://{address}")).expect("lease worker URL"),
    );
    let acquired = lease_client
        .acquire(ManagedLeaseAcquireRequest {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        })
        .await
        .expect("lease acquired");
    nats.lease_intent
        .store_lease(acquired.lease.clone(), acquired.bundle)
        .await
        .expect("record-only lease stored");
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, auto_hostname_deploy_request()).await)
        .await
        .expect("deploy operation accepted");
    let mut dataplane = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();

    run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client,
            managed_certificate_wait: ManagedCertificateWaitPolicy::new(
                Duration::from_secs(1),
                Duration::from_millis(5),
            ),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut dataplane,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("deploy waits for ready certificate and runs");

    assert_eq!(runtime.requests.len(), 1);
    assert!(matches!(
        nats.lease_intent.load().await.expect("lease intent reads"),
        ManagedLeaseIntent::Auto { state }
            if matches!(*state, AutoLeaseState::Ready { .. })
    ));
    assert_eq!(
        waiting_for_managed_certificate_event_count(&controllers).await,
        1
    );
    worker.abort();
}

#[tokio::test]
async fn auto_hostname_deploy_fails_typed_when_certificate_stays_pending() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let (lease_client, request_count, server) = always_pending_lease_client().await;
    nats.lease_intent
        .store_lease(pending_lease_record(), None)
        .await
        .expect("record-only lease stored");
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, auto_hostname_deploy_request()).await)
        .await
        .expect("deploy operation accepted");
    let mut dataplane = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_should_not_start"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();

    let error = run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client,
            managed_certificate_wait: ManagedCertificateWaitPolicy::new(
                Duration::from_millis(80),
                Duration::from_millis(5),
            ),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut dataplane,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
        },
        Duration::from_secs(5),
    )
    .await
    .expect_err("pending certificate fails deploy at the deadline");

    assert!(matches!(error, DeployOperationRunError::LoadFacts { .. }));
    assert!(runtime.requests.is_empty());
    assert!(request_count.load(Ordering::SeqCst) >= 2);
    assert!(matches!(
        controllers
            .repository()
            .get(&operation_id("op_123"))
            .await
            .expect("status reads"),
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Failed {
                failure: DeployOperationFailure::CertificatePending {
                    last_error: Some(ManagedCertificateIssuanceFailureKind::ValidationTimeout),
                },
            },
            ..
        })
    ));
    assert_eq!(
        waiting_for_managed_certificate_event_count(&controllers).await,
        1
    );
    server.abort();
}

#[tokio::test]
async fn deploy_without_auto_hostname_does_not_poll_record_only_lease() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(nats.machine_a.clone(), machine_id("machine_a")).await;
    let (lease_client, request_count, server) = always_pending_lease_client().await;
    nats.lease_intent
        .store_lease(pending_lease_record(), None)
        .await
        .expect("record-only lease stored");
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request(1)).await)
        .await
        .expect("deploy operation accepted");
    let mut dataplane = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();

    run_deploy_operation(
        accepted,
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        DeployOperationStores {
            intent_change_client: nats.client.clone(),
            namespace_intent: nats.namespace_intent.clone(),
            lease_intent: nats.lease_intent.clone(),
            lease_client,
            managed_certificate_wait: ManagedCertificateWaitPolicy::new(
                Duration::from_millis(80),
                Duration::from_millis(5),
            ),
            controllers: controllers.clone(),
        },
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            dataplane: &mut dataplane,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("deploy without auto hostname runs immediately");

    assert_eq!(runtime.requests.len(), 1);
    assert_eq!(request_count.load(Ordering::SeqCst), 0);
    server.abort();
}

async fn waiting_for_managed_certificate_event_count(controllers: &OperationControllers) -> usize {
    controllers
        .repository()
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_123"),
            start_sequence: EventSequence::try_new(1).expect("valid start sequence"),
            limit: OperationEventReplayLimit::try_new(100).expect("valid replay limit"),
        })
        .await
        .expect("operation events replay")
        .events
        .into_iter()
        .filter(|event| {
            matches!(
                event.event,
                OperationEvent::DeployWaitingForManagedCertificate { .. }
            )
        })
        .count()
}

async fn always_pending_lease_client()
-> (LeaseClient, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind pending lease worker");
    let address = listener.local_addr().expect("pending worker address");
    let request_count = Arc::new(AtomicUsize::new(0));
    let server_count = Arc::clone(&request_count);
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept bundle request");
            let mut request = [0_u8; 2048];
            let read = stream
                .read(&mut request)
                .await
                .expect("read bundle request");
            assert!(read > 0, "bundle request is not empty");
            server_count.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"status":"pending","last_error":"validation_timeout"}"#;
            let response = format!(
                "HTTP/1.1 202 Accepted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write pending response");
        }
    });
    let client = LeaseClient::new(
        LeaseWorkerUrl::try_new(format!("http://{address}")).expect("pending worker URL"),
    );
    (client, request_count, server)
}

fn pending_lease_record() -> ManagedLeaseRecord {
    ManagedLeaseRecord::try_new(
        ManagedLeaseName::try_new("cluster-one").expect("lease name"),
        LeaseBearerToken::try_new("lease-token").expect("lease token"),
        LeaseIssuedAt::try_new(1_700_000_000).expect("issued at"),
        LeaseExpiresAt::try_new(1_707_776_000).expect("expires at"),
    )
    .expect("lease record")
}

fn auto_hostname_deploy_request() -> DeployRequest {
    let mut request = deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.routes.push(DeployRoute {
        target: DeployRouteTarget::AutoHostname {
            port: route_port(443),
        },
        endpoint_port: route_port(8080),
    });
    request
}
