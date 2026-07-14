use super::*;
use ployz_core::cert::{
    AutoLeaseState, LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt, ManagedCertBundle,
    ManagedLeaseAddressSet, ManagedLeaseIntent, ManagedLeaseName, ManagedLeaseRecord,
};
use ployz_core::ops::{
    EventSequence, ManagedPublicUrlPending, ManagedPublicUrlPendingStage, OperationEvent,
    OperationEventReplayLimit, OperationEventReplayRequest,
};
use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::test]
async fn auto_hostname_deploy_observes_managed_public_url_becoming_ready() {
    let nats = test_nats().await;
    let lease = valid_lease_record();
    nats.lease_intent
        .store_successful_lease_application(lease.clone(), None, gateway_address_set())
        .await
        .expect("record-only lease stored");
    let _facts = start_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, auto_hostname_deploy_request()).await)
        .await
        .expect("deploy operation accepted");
    let lease_intent = nats.lease_intent.clone();
    let readiness_controllers = controllers.clone();
    let readiness = tokio::spawn(async move {
        loop {
            if waiting_for_managed_public_url_event_count(
                &readiness_controllers,
                ManagedPublicUrlPendingStage::Certificate,
            )
            .await
                == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        lease_intent
            .store_successful_lease_application(
                lease.clone(),
                Some(valid_bundle(&lease)),
                gateway_address_set(),
            )
            .await
            .expect("ready lease stored");
    });

    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    run_deploy_operation(
        accepted,
        deploy_stores(
            &nats,
            controllers.clone(),
            ManagedPublicUrlWaitPolicy::new(Duration::from_secs(1), Duration::from_millis(5)),
        ),
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("deploy observes ready managed public URL");

    readiness.await.expect("readiness task completes");
    assert!(matches!(
        nats.lease_intent.load().await.expect("lease intent reads"),
        ManagedLeaseIntent::Auto { state }
            if matches!(*state, AutoLeaseState::Ready { .. })
    ));
    assert_eq!(
        waiting_for_managed_public_url_event_count(
            &controllers,
            ManagedPublicUrlPendingStage::Certificate,
        )
        .await,
        1
    );
}

#[tokio::test]
async fn auto_hostname_deploy_fails_typed_when_certificate_stays_pending() {
    let nats = test_nats().await;
    nats.lease_intent
        .store_successful_lease_application(valid_lease_record(), None, gateway_address_set())
        .await
        .expect("record-only lease stored");

    let run = run_auto_hostname_deploy(
        &nats,
        ManagedPublicUrlWaitPolicy::new(Duration::from_millis(40), Duration::from_millis(5)),
    )
    .await;
    let error = run
        .result
        .expect_err("pending certificate reaches the deadline");

    assert!(matches!(error, DeployOperationRunError::LoadFacts { .. }));
    assert_pending_failure(
        &run.controllers,
        ManagedPublicUrlPending::Certificate { last_error: None },
    )
    .await;
}

#[tokio::test]
async fn auto_hostname_deploy_treats_expired_lease_as_lease_pending() {
    let nats = test_nats().await;
    let lease = expired_lease_record();
    nats.lease_intent
        .store_successful_lease_application(
            lease.clone(),
            Some(expired_bundle(&lease)),
            gateway_address_set(),
        )
        .await
        .expect("expired lease stored");

    let run = run_auto_hostname_deploy(
        &nats,
        ManagedPublicUrlWaitPolicy::new(Duration::from_millis(40), Duration::from_millis(5)),
    )
    .await;
    run.result.expect_err("expired lease reaches the deadline");

    assert_pending_failure(&run.controllers, ManagedPublicUrlPending::Lease).await;
}

#[tokio::test]
async fn auto_hostname_deploy_treats_expired_bundle_as_certificate_pending() {
    let nats = test_nats().await;
    let lease = valid_lease_record();
    nats.lease_intent
        .store_successful_lease_application(
            lease.clone(),
            Some(expired_bundle(&lease)),
            gateway_address_set(),
        )
        .await
        .expect("expired bundle stored");

    let run = run_auto_hostname_deploy(
        &nats,
        ManagedPublicUrlWaitPolicy::new(Duration::from_millis(40), Duration::from_millis(5)),
    )
    .await;
    run.result.expect_err("expired bundle reaches the deadline");

    assert_pending_failure(
        &run.controllers,
        ManagedPublicUrlPending::Certificate { last_error: None },
    )
    .await;
}

#[tokio::test]
async fn auto_hostname_deploy_waits_boundedly_for_unacquired_lease() {
    let nats = test_nats().await;

    let run = run_auto_hostname_deploy(
        &nats,
        ManagedPublicUrlWaitPolicy::new(Duration::from_millis(40), Duration::from_millis(5)),
    )
    .await;
    run.result
        .expect_err("unacquired lease reaches the deadline");

    assert_pending_failure(&run.controllers, ManagedPublicUrlPending::Lease).await;
}

#[tokio::test]
async fn auto_hostname_deploy_waits_for_applied_gateway_addresses() {
    let nats = test_nats().await;
    let lease = valid_lease_record();
    nats.lease_intent
        .store_lease(lease.clone(), Some(valid_bundle(&lease)))
        .await
        .expect("ready lease stored");

    let run = run_auto_hostname_deploy(
        &nats,
        ManagedPublicUrlWaitPolicy::new(Duration::from_millis(40), Duration::from_millis(5)),
    )
    .await;
    run.result
        .expect_err("missing applied gateway addresses reaches the deadline");

    assert_pending_failure(&run.controllers, ManagedPublicUrlPending::GatewayAddresses).await;
}

#[tokio::test]
async fn deploy_without_auto_hostname_ignores_pending_managed_public_url() {
    let nats = test_nats().await;
    let _facts = start_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    nats.lease_intent
        .store_lease(valid_lease_record(), None)
        .await
        .expect("record-only lease stored");
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, deploy_request(1)).await)
        .await
        .expect("deploy operation accepted");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();

    run_deploy_operation(
        accepted,
        deploy_stores(
            &nats,
            controllers,
            ManagedPublicUrlWaitPolicy::new(Duration::from_millis(40), Duration::from_millis(5)),
        ),
        DeployOperationPorts {
            facts_reader: &facts_reader(&nats.client, Duration::from_secs(5)),
            intent_reader: &intent_reader(&nats.client, Duration::from_secs(5)),
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
        },
        Duration::from_secs(5),
    )
    .await
    .expect("deploy without auto hostname runs immediately");

    assert_eq!(runtime.requests.len(), 1);
}

async fn run_auto_hostname_deploy(
    nats: &TestNats,
    policy: ManagedPublicUrlWaitPolicy,
) -> AutoDeployRun {
    let _facts = start_facts_subscription(
        nats.machine_a.clone(),
        nats.client.clone(),
        machine_id("machine_a"),
    )
    .await;
    let controllers = operation_controllers(nats.client.clone()).await;
    let accepted = controllers
        .submit_deploy(deploy_submit_command(&controllers, auto_hostname_deploy_request()).await)
        .await
        .expect("deploy operation accepted");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();
    let facts_reader = facts_reader(&nats.client, Duration::from_secs(5));
    let intent_reader = intent_reader(&nats.client, Duration::from_secs(5));
    let result = run_deploy_operation(
        accepted,
        deploy_stores(nats, controllers.clone(), policy),
        DeployOperationPorts {
            facts_reader: &facts_reader,
            intent_reader: &intent_reader,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
        },
        Duration::from_secs(5),
    )
    .await
    .map(|_| ());
    AutoDeployRun {
        result,
        controllers,
    }
}

struct AutoDeployRun {
    result: Result<(), DeployOperationRunError>,
    controllers: OperationControllers,
}

fn deploy_stores(
    nats: &TestNats,
    controllers: OperationControllers,
    policy: ManagedPublicUrlWaitPolicy,
) -> DeployOperationStores {
    DeployOperationStores {
        intent_change_client: nats.client.clone(),
        namespace_intent: nats.namespace_intent.clone(),
        lease_intent: nats.lease_intent.clone(),
        managed_public_url_wait: policy,
        controllers,
    }
}

async fn assert_pending_failure(
    controllers: &OperationControllers,
    expected: ManagedPublicUrlPending,
) {
    assert!(matches!(
        controllers
            .repository()
            .get(&operation_id("op_123"))
            .await
            .expect("status reads"),
        Some(OperationStatus::Deploy {
            state: DeployOperationState::Failed {
                failure: DeployOperationFailure::ManagedPublicUrlPending { pending },
            },
            ..
        }) if pending == expected
    ));
    assert_eq!(
        waiting_for_managed_public_url_event_count(controllers, expected.stage()).await,
        1
    );
}

async fn waiting_for_managed_public_url_event_count(
    controllers: &OperationControllers,
    expected_stage: ManagedPublicUrlPendingStage,
) -> usize {
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
                OperationEvent::DeployWaitingForManagedPublicUrl { stage, .. }
                    if stage == expected_stage
            )
        })
        .count()
}

fn gateway_address_set() -> ManagedLeaseAddressSet {
    ManagedLeaseAddressSet::new(
        vec!["203.0.113.8".parse::<Ipv4Addr>().expect("IPv4")],
        Vec::new(),
    )
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_secs()
}

fn valid_lease_record() -> ManagedLeaseRecord {
    lease_record(now_seconds() - 60, now_seconds() + 3_600)
}

fn expired_lease_record() -> ManagedLeaseRecord {
    lease_record(now_seconds() - 120, now_seconds() - 60)
}

fn lease_record(issued_at: u64, expires_at: u64) -> ManagedLeaseRecord {
    ManagedLeaseRecord::try_new(
        ManagedLeaseName::try_new("cluster-one").expect("lease name"),
        LeaseBearerToken::try_new("lease-token").expect("lease token"),
        LeaseIssuedAt::try_new(issued_at).expect("issued at"),
        LeaseExpiresAt::try_new(expires_at).expect("expires at"),
    )
    .expect("lease record")
}

fn valid_bundle(lease: &ManagedLeaseRecord) -> ManagedCertBundle {
    bundle(lease, now_seconds() - 60, now_seconds() + 3_600)
}

fn expired_bundle(lease: &ManagedLeaseRecord) -> ManagedCertBundle {
    bundle(lease, now_seconds() - 120, now_seconds() - 60)
}

fn bundle(lease: &ManagedLeaseRecord, issued_at: u64, expires_at: u64) -> ManagedCertBundle {
    ManagedCertBundle::try_new(
        lease.name.clone(),
        lease.name.wildcard_and_apex(),
        "certificate".to_owned(),
        "private-key".to_owned(),
        LeaseIssuedAt::try_new(issued_at).expect("issued at"),
        LeaseExpiresAt::try_new(expires_at).expect("expires at"),
    )
    .expect("certificate bundle")
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
