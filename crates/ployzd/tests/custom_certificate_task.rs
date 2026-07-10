use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, CertificateArtifactPushOk,
    CertificateArtifactPushRequest, CertificateArtifactPushResponse,
    CertificateChallengeApplicationStatus, CertificateChallengeStatusOk,
    CertificateChallengeStatusRequest, CertificateChallengeStatusResponse,
};
use ployz_core::ids::MachineId;
use ployz_core::install::{AbsoluteInstallPath, InstallSha256Digest};
use ployz_core::machine_rpc::MachineRpcResponse;
use ployz_core::ops::{
    CertOperationFailure, CertOperationState, CertificateProvisionFailure, FailureMessage,
    OperationStatus, RouteHostname,
};
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_test_support::ids::{cert_id, operation_id, route_hostname};
use ployzd::certificate::task::{
    CertificateRenewalOutcome, recover_unfinished_operations, run_once_at,
};
use ployzd::certificate::{
    AcmeIssueContext, AcmeIssuer, AcmeIssuerError, CertificateManager, CertificateManagerConfig,
    GatewayCertificateTarget, IssuedCertificate,
};
use ployzd::core_store::CoreStore;
use ployzd::intent::certificate_intent::CertificateIntentStore;
use ployzd::operations::log::{CertOperationSubmission, OperationRepository};
use rcgen::{CertificateParams, CertifiedKey, KeyPair};
use time::OffsetDateTime;

#[tokio::test]
async fn failed_due_renewal_keeps_the_active_certificate_and_records_terminal_evidence() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let issued = fixture_certificate("localhost");
    let issuer = Arc::new(FakeAcmeIssuer::new([
        Ok(issued),
        Err(AcmeIssuerError::Validation {
            message: "stub CA rejected renewal".to_owned(),
        }),
    ]));
    let manager = CertificateManager::with_issuer(
        core_store.clone(),
        nats.controller.clone(),
        CertificateManagerConfig {
            state_dir: state_dir.path().to_path_buf(),
            ..CertificateManagerConfig::for_core_db(state_dir.path())
        },
        issuer,
    );
    let hostname = route_hostname("localhost");
    let original = manager
        .ensure(
            &operation_id("op_deploy_initial"),
            &hostname,
            &gateway_targets(),
        )
        .await
        .expect("initial certificate");

    let outcome = run_once_at(&manager, &gateway_targets(), u64::MAX)
        .await
        .expect("renewal tick");
    let retained = manager
        .ensure(
            &operation_id("op_deploy_retained"),
            &hostname,
            &gateway_targets(),
        )
        .await
        .expect("retained certificate");
    let statuses = OperationRepository::open(core_store, nats.controller)
        .operation_statuses()
        .await
        .expect("operation statuses");

    assert_eq!(
        outcome,
        CertificateRenewalOutcome::Attempted {
            attempted: 1,
            failed: 1,
        }
    );
    assert_eq!(retained, original);
    assert!(statuses.iter().any(|status| matches!(
        status,
        OperationStatus::Cert {
            state: CertOperationState::Failed { .. },
            ..
        }
    )));
}

#[tokio::test]
async fn renewal_dns_failure_records_terminal_evidence_with_retained_metadata() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let manager = CertificateManager::with_issuer(
        core_store.clone(),
        nats.controller.clone(),
        test_config(state_dir.path()),
        Arc::new(FakeAcmeIssuer::new([Ok(fixture_certificate("localhost"))])),
    );
    let hostname = route_hostname("localhost");
    let active = manager
        .ensure(
            &operation_id("op_deploy_initial"),
            &hostname,
            &gateway_targets(),
        )
        .await
        .expect("initial certificate");

    let outcome = run_once_at(
        &manager,
        &[GatewayCertificateTarget {
            machine_id: gateway_machine_id(),
            public_ips: vec![IpAddr::from([192, 0, 2, 10])],
        }],
        u64::MAX,
    )
    .await
    .expect("renewal tick");

    assert_eq!(
        outcome,
        CertificateRenewalOutcome::Attempted {
            attempted: 1,
            failed: 1,
        }
    );
    let statuses = OperationRepository::open(core_store, nats.controller)
        .operation_statuses()
        .await
        .expect("operation statuses");
    assert!(statuses.iter().any(|status| {
        let OperationStatus::Cert {
            state: CertOperationState::Failed { failure },
            ..
        } = status
        else {
            return false;
        };
        matches!(
            failure.failure(),
            CertificateProvisionFailure::DnsPreflight { .. }
        ) && failure.retained_active_cert() == Some(&active)
    }));
}

#[tokio::test]
async fn cancelled_waiter_leaves_cleanup_and_terminal_evidence_to_the_owned_task() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let issuer = Arc::new(BlockingFailureIssuer::default());
    let manager = CertificateManager::with_issuer(
        core_store.clone(),
        nats.controller.clone(),
        test_config(state_dir.path()),
        issuer.clone(),
    );
    let hostname = route_hostname("localhost");
    let waiter_manager = manager.clone();
    let waiter = tokio::spawn(async move {
        waiter_manager
            .ensure(
                &operation_id("op_deploy_cancelled"),
                &hostname,
                &gateway_targets(),
            )
            .await
    });
    issuer.published.notified().await;

    waiter.abort();
    issuer.release.notify_one();

    let repository = OperationRepository::open(core_store.clone(), nats.controller);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if repository
                .operation_statuses()
                .await
                .expect("operation statuses")
                .iter()
                .any(OperationStatus::is_terminal)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("issuance reaches terminal state");
    assert!(
        CertificateIntentStore::new(core_store)
            .challenges()
            .await
            .expect("challenge intent")
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_ensure_calls_share_one_certificate_issuance() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let issuer = Arc::new(FakeAcmeIssuer::new([Ok(fixture_certificate("localhost"))]));
    let manager = CertificateManager::with_issuer(
        core_store,
        nats.controller,
        test_config(state_dir.path()),
        issuer.clone(),
    );
    let hostname = route_hostname("localhost");
    let targets = gateway_targets();
    let first_operation_id = operation_id("op_deploy_first");
    let second_operation_id = operation_id("op_deploy_second");

    let (first, second) = tokio::join!(
        manager.ensure(&first_operation_id, &hostname, &targets),
        manager.ensure(&second_operation_id, &hostname, &targets)
    );

    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_eq!(issuer.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn expired_active_certificate_is_reissued() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let initial_now = system_now_seconds();
    let issuer = Arc::new(FakeAcmeIssuer::new([
        Ok(fixture_certificate_at(
            "localhost",
            initial_now - 60,
            initial_now + 1,
        )),
        Ok(fixture_certificate_at(
            "localhost",
            initial_now - 60,
            initial_now + 3_600,
        )),
    ]));
    let now = Arc::new(AtomicU64::new(initial_now));
    let clock = {
        let now = Arc::clone(&now);
        Arc::new(move || now.load(Ordering::Relaxed))
    };
    let manager = CertificateManager::with_issuer_and_time(
        core_store,
        nats.controller,
        test_config(state_dir.path()),
        issuer.clone(),
        clock,
    );
    let hostname = route_hostname("localhost");
    manager
        .ensure(
            &operation_id("op_deploy_initial"),
            &hostname,
            &gateway_targets(),
        )
        .await
        .expect("initial certificate");
    now.store(initial_now + 2, Ordering::Relaxed);

    manager
        .ensure(
            &operation_id("op_deploy_reissue"),
            &hostname,
            &gateway_targets(),
        )
        .await
        .expect("replacement certificate");

    assert_eq!(issuer.calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn certificate_for_another_hostname_is_rejected_before_commit() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let manager = CertificateManager::with_issuer(
        core_store.clone(),
        nats.controller,
        test_config(state_dir.path()),
        Arc::new(FakeAcmeIssuer::new([Ok(fixture_certificate(
            "other.example.com",
        ))])),
    );

    let error = manager
        .ensure(
            &operation_id("op_deploy_mismatch"),
            &route_hostname("localhost"),
            &gateway_targets(),
        )
        .await
        .expect_err("mismatched certificate is rejected");

    assert!(matches!(
        error,
        CertificateProvisionFailure::AcmeValidation { .. }
    ));
    assert!(
        CertificateIntentStore::new(core_store)
            .active_certificates()
            .await
            .expect("certificate intent")
            .is_empty()
    );
}

#[tokio::test]
async fn future_dated_certificate_is_rejected_before_commit() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let now = system_now_seconds();
    let manager = CertificateManager::with_issuer_and_time(
        core_store.clone(),
        nats.controller,
        test_config(state_dir.path()),
        Arc::new(FakeAcmeIssuer::new([Ok(fixture_certificate_at(
            "localhost",
            now + 60,
            now + 120,
        ))])),
        Arc::new(move || now),
    );

    let error = manager
        .ensure(
            &operation_id("op_deploy_future_cert"),
            &route_hostname("localhost"),
            &gateway_targets(),
        )
        .await
        .expect_err("future certificate is rejected");

    assert!(matches!(
        error,
        CertificateProvisionFailure::AcmeValidation { .. }
    ));
    assert!(
        CertificateIntentStore::new(core_store)
            .active_certificates()
            .await
            .expect("certificate intent")
            .is_empty()
    );
}

#[tokio::test]
async fn startup_recovery_clears_stale_challenges_and_fails_unfinished_operations() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let hostname = route_hostname("localhost");
    let cert_id = cert_id("cert_localhost");
    let operation_id = operation_id("op_cert_recovery");
    let intent = CertificateIntentStore::new(core_store.clone());
    intent
        .store_challenge(challenge(hostname))
        .await
        .expect("store stale challenge");
    let repository = OperationRepository::open(core_store.clone(), nats.controller.clone());
    repository
        .submit_cert(CertOperationSubmission {
            operation_id: operation_id.clone(),
            cert_id: cert_id.clone(),
        })
        .await
        .expect("submit unfinished certificate operation");
    let manager = CertificateManager::with_issuer(
        core_store.clone(),
        nats.controller,
        test_config(state_dir.path()),
        Arc::new(BlockingFailureIssuer::default()),
    );

    recover_unfinished_operations(&manager)
        .await
        .expect("recover unfinished certificate operation");

    assert!(
        intent
            .challenges()
            .await
            .expect("challenge intent")
            .is_empty()
    );
    let statuses = repository
        .operation_statuses()
        .await
        .expect("operation statuses");
    let recovered = statuses
        .iter()
        .find(|status| status.id() == &operation_id)
        .expect("recovered operation status");
    let OperationStatus::Cert {
        cert_id: status_cert_id,
        state: CertOperationState::Failed { failure },
        ..
    } = recovered
    else {
        panic!("recovered certificate operation is failed");
    };
    assert_eq!(status_cert_id, &cert_id);
    assert_eq!(failure.cert_id(), &cert_id);
    assert!(matches!(
        failure.failure(),
        CertificateProvisionFailure::AcmeValidation { .. }
    ));
    assert_eq!(failure.retained_active_cert(), None);
}

#[tokio::test]
async fn cached_certificate_still_requires_dns_preflight() {
    let (nats, _gateway) = test_nats_with_gateway(true).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let issuer = Arc::new(FakeAcmeIssuer::new([Ok(fixture_certificate("localhost"))]));
    let manager = CertificateManager::with_issuer(
        core_store,
        nats.controller,
        test_config(state_dir.path()),
        issuer.clone(),
    );
    let hostname = route_hostname("localhost");
    manager
        .ensure(
            &operation_id("op_deploy_initial"),
            &hostname,
            &gateway_targets(),
        )
        .await
        .expect("initial certificate");

    let error = manager
        .ensure(
            &operation_id("op_deploy_wrong_dns"),
            &hostname,
            &[GatewayCertificateTarget {
                machine_id: gateway_machine_id(),
                public_ips: vec![IpAddr::from([192, 0, 2, 10])],
            }],
        )
        .await
        .expect_err("cached certificate does not bypass DNS preflight");

    assert!(matches!(
        error,
        CertificateProvisionFailure::DnsPreflight { .. }
    ));
    assert_eq!(issuer.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn failed_completion_projection_cannot_leave_active_metadata() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let repository = OperationRepository::open(core_store.clone(), nats.controller);
    let operation_id = operation_id("op_cert_atomic_failure");
    repository
        .submit_cert(CertOperationSubmission {
            operation_id: operation_id.clone(),
            cert_id: cert_id("cert_expected"),
        })
        .await
        .expect("submit certificate operation");

    repository
        .activate_cert(
            &operation_id,
            active_cert("cert_other", "other.example.com"),
        )
        .await
        .expect_err("mismatched completion is rejected");

    assert!(
        CertificateIntentStore::new(core_store)
            .active_certificates()
            .await
            .expect("active metadata")
            .is_empty()
    );
}

#[tokio::test]
async fn atomic_activation_cannot_be_rewritten_as_failed() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let repository = OperationRepository::open(core_store.clone(), nats.controller);
    let operation_id = operation_id("op_cert_atomic_success");
    let active = active_cert("cert_example", "example.com");
    repository
        .submit_cert(CertOperationSubmission {
            operation_id: operation_id.clone(),
            cert_id: active.cert_id.clone(),
        })
        .await
        .expect("submit certificate operation");
    repository
        .activate_cert(&operation_id, active.clone())
        .await
        .expect("activate certificate");
    let failure = CertOperationFailure::try_new(
        active.cert_id.clone(),
        CertificateProvisionFailure::AcmeValidation {
            message: FailureMessage::try_new("late failure").expect("failure message"),
        },
        Some(active.clone()),
    )
    .expect("failure evidence");

    repository
        .record_cert_failed(&operation_id, failure)
        .await
        .expect_err("completed operation is terminal");

    assert_eq!(
        CertificateIntentStore::new(core_store.clone())
            .active_for_hostname(&active.hostname)
            .await
            .expect("active metadata"),
        Some(active)
    );
    assert!(matches!(
        repository.get(&operation_id).await.expect("status"),
        Some(OperationStatus::Cert {
            state: CertOperationState::Completed { .. },
            ..
        })
    ));
}

#[tokio::test]
async fn renewal_precondition_failure_records_retained_metadata() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let manager = CertificateManager::with_issuer(
        core_store.clone(),
        nats.controller.clone(),
        test_config(state_dir.path()),
        Arc::new(FakeAcmeIssuer::new([])),
    );
    let active = active_cert("cert_example", "example.com");

    manager
        .record_renewal_failure(
            active.clone(),
            CertificateProvisionFailure::DnsPreflight {
                message: FailureMessage::try_new("gateway roster unavailable")
                    .expect("failure message"),
            },
        )
        .await
        .expect("record renewal failure");

    let statuses = OperationRepository::open(core_store, nats.controller)
        .operation_statuses()
        .await
        .expect("operation statuses");
    assert!(statuses.iter().any(|status| {
        let OperationStatus::Cert {
            state: CertOperationState::Failed { failure },
            ..
        } = status
        else {
            return false;
        };
        failure.retained_active_cert() == Some(&active)
    }));
}

#[tokio::test]
async fn acme_validation_waits_for_exact_gateway_challenge_application() {
    let (nats, gateway) = test_nats_with_gateway(false).await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let issuer = Arc::new(ReadinessProbeIssuer {
        issued: fixture_certificate("localhost"),
        validation_started: std::sync::atomic::AtomicBool::new(false),
    });
    let manager = CertificateManager::with_issuer(
        core_store,
        nats.controller,
        test_config(state_dir.path()),
        issuer.clone(),
    );
    let ensure = tokio::spawn(async move {
        manager
            .ensure(
                &operation_id("op_deploy_readiness"),
                &route_hostname("localhost"),
                &gateway_targets(),
            )
            .await
    });

    gateway.wait_for_challenge_request().await;
    assert!(!ensure.is_finished());
    assert!(!issuer.validation_started.load(Ordering::Acquire));

    gateway.apply_challenge();
    tokio::time::timeout(Duration::from_secs(1), ensure)
        .await
        .expect("issuance observes applied challenge")
        .expect("issuance task")
        .expect("certificate issuance");
    assert!(issuer.validation_started.load(Ordering::Acquire));
}

struct ReadinessProbeIssuer {
    issued: IssuedCertificate,
    validation_started: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl AcmeIssuer for ReadinessProbeIssuer {
    async fn issue_http01(
        &self,
        context: &AcmeIssueContext,
        hostname: &RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        context
            .publish_challenge(challenge(hostname.clone()))
            .await?;
        self.validation_started.store(true, Ordering::Release);
        context.validation_started().await?;
        Ok(self.issued.clone())
    }
}

struct FakeAcmeIssuer {
    results: Mutex<VecDeque<Result<IssuedCertificate, AcmeIssuerError>>>,
    calls: AtomicUsize,
}

impl FakeAcmeIssuer {
    fn new(results: impl IntoIterator<Item = Result<IssuedCertificate, AcmeIssuerError>>) -> Self {
        Self {
            results: Mutex::new(results.into_iter().collect()),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AcmeIssuer for FakeAcmeIssuer {
    async fn issue_http01(
        &self,
        context: &AcmeIssueContext,
        hostname: &RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        context
            .publish_challenge(challenge(hostname.clone()))
            .await?;
        context.validation_started().await?;
        self.results
            .lock()
            .expect("issuer results lock")
            .pop_front()
            .expect("configured issuer result")
    }
}

#[derive(Default)]
struct BlockingFailureIssuer {
    published: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[async_trait]
impl AcmeIssuer for BlockingFailureIssuer {
    async fn issue_http01(
        &self,
        context: &AcmeIssueContext,
        hostname: &RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        context
            .publish_challenge(challenge(hostname.clone()))
            .await?;
        context.validation_started().await?;
        self.published.notify_one();
        self.release.notified().await;
        Err(AcmeIssuerError::Validation {
            message: "cancelled caller test failure".to_owned(),
        })
    }
}

fn fixture_certificate(hostname: &str) -> IssuedCertificate {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed([hostname.to_owned()]).expect("fixture certificate");
    IssuedCertificate {
        certificate_chain_pem: cert.pem(),
        private_key_pem: signing_key.serialize_pem(),
    }
}

fn fixture_certificate_at(hostname: &str, not_before: u64, not_after: u64) -> IssuedCertificate {
    let mut params = CertificateParams::new(vec![hostname.to_owned()]).expect("certificate params");
    params.not_before =
        OffsetDateTime::from_unix_timestamp(i64::try_from(not_before).expect("not before fits"))
            .expect("not before timestamp");
    params.not_after =
        OffsetDateTime::from_unix_timestamp(i64::try_from(not_after).expect("not after fits"))
            .expect("not after timestamp");
    let key = KeyPair::generate().expect("private key");
    let certificate = params.self_signed(&key).expect("certificate");
    IssuedCertificate {
        certificate_chain_pem: certificate.pem(),
        private_key_pem: key.serialize_pem(),
    }
}

fn system_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_secs()
}

fn challenge(hostname: RouteHostname) -> AcmeHttp01Challenge {
    AcmeHttp01Challenge::try_new(
        hostname,
        AcmeChallengeToken::try_new("stub-token").expect("token"),
        AcmeChallengeValue::try_new("stub-token.account-thumbprint").expect("value"),
        AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
    )
    .expect("challenge")
}

fn loopback_ips() -> [IpAddr; 2] {
    [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
}

fn gateway_machine_id() -> MachineId {
    MachineId::try_new("machine_gateway").expect("gateway machine id")
}

fn gateway_targets() -> Vec<GatewayCertificateTarget> {
    vec![GatewayCertificateTarget {
        machine_id: gateway_machine_id(),
        public_ips: loopback_ips().to_vec(),
    }]
}

async fn test_nats_with_gateway(
    challenge_applied: bool,
) -> (ployz_test_support::nats::TestNats, TestGateway) {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[gateway_machine_id()]).await;
    let gateway = TestGateway::start_with_challenge_status(&nats, challenge_applied).await;
    (nats, gateway)
}

fn active_cert(cert: &str, hostname: &str) -> ActiveCertState {
    let digest = InstallSha256Digest::try_new("a".repeat(64)).expect("bundle digest");
    ActiveCertState {
        cert_id: cert_id(cert),
        hostname: route_hostname(hostname),
        bundle_ref: CertBundleRef::for_bundle(
            &digest,
            &AbsoluteInstallPath::try_new(format!("/var/lib/ployz/{cert}.bundle"))
                .expect("bundle path"),
        )
        .expect("bundle reference"),
        validity: CertValidityWindow::try_new(
            CertValidAt::try_new(1).expect("not before"),
            CertValidAt::try_new(2).expect("not after"),
        )
        .expect("validity"),
    }
}

struct TestGateway {
    tasks: Vec<tokio::task::JoinHandle<()>>,
    challenge_applied: Arc<std::sync::atomic::AtomicBool>,
    challenge_requests: Arc<tokio::sync::Notify>,
}

impl TestGateway {
    async fn start_with_challenge_status(
        nats: &ployz_test_support::nats::TestNats,
        applied: bool,
    ) -> Self {
        let machine_id = gateway_machine_id();
        let client = nats.machine_client(&machine_id).await;
        let challenge_applied = Arc::new(std::sync::atomic::AtomicBool::new(applied));
        let challenge_requests = Arc::new(tokio::sync::Notify::new());
        let mut pushes = client
            .subscribe(machine_service(
                &machine_id,
                MachineServiceEndpoint::CertificateArtifactPush,
            ))
            .await
            .expect("subscribe artifact push");
        let mut challenges = client
            .subscribe(machine_service(
                &machine_id,
                MachineServiceEndpoint::CertificateChallengeStatus,
            ))
            .await
            .expect("subscribe challenge status");
        client.flush().await.expect("flush gateway subscriptions");

        let push_client = client.clone();
        let push_machine_id = machine_id.clone();
        let push_task = tokio::spawn(async move {
            while let Some(message) = pushes.next().await {
                let Some(reply) = message.reply.clone() else {
                    continue;
                };
                let Ok(request) =
                    serde_json::from_slice::<CertificateArtifactPushRequest>(&message.payload)
                else {
                    continue;
                };
                let response: CertificateArtifactPushResponse =
                    MachineRpcResponse::Ok(CertificateArtifactPushOk {
                        machine_id: push_machine_id.clone(),
                        cert_id: request.bundle.active_cert().cert_id.clone(),
                        digest: request.expected_digest,
                    });
                let _ = push_client
                    .publish(
                        reply,
                        serde_json::to_vec(&response)
                            .expect("encode artifact response")
                            .into(),
                    )
                    .await;
            }
        });

        let challenge_client = client.clone();
        let responder_applied = Arc::clone(&challenge_applied);
        let responder_requests = Arc::clone(&challenge_requests);
        let challenge_task = tokio::spawn(async move {
            while let Some(message) = challenges.next().await {
                let Some(reply) = message.reply.clone() else {
                    continue;
                };
                if serde_json::from_slice::<CertificateChallengeStatusRequest>(&message.payload)
                    .is_err()
                {
                    continue;
                }
                let response: CertificateChallengeStatusResponse =
                    MachineRpcResponse::Ok(CertificateChallengeStatusOk {
                        machine_id: machine_id.clone(),
                        application: if responder_applied.load(Ordering::Acquire) {
                            CertificateChallengeApplicationStatus::Applied
                        } else {
                            CertificateChallengeApplicationStatus::NotApplied
                        },
                    });
                let _ = challenge_client
                    .publish(
                        reply,
                        serde_json::to_vec(&response)
                            .expect("encode challenge response")
                            .into(),
                    )
                    .await;
                responder_requests.notify_one();
            }
        });

        Self {
            tasks: vec![push_task, challenge_task],
            challenge_applied,
            challenge_requests,
        }
    }

    async fn wait_for_challenge_request(&self) {
        self.challenge_requests.notified().await;
    }

    fn apply_challenge(&self) {
        self.challenge_applied.store(true, Ordering::Release);
    }
}

impl Drop for TestGateway {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn test_config(state_dir: &std::path::Path) -> CertificateManagerConfig {
    CertificateManagerConfig {
        state_dir: state_dir.to_path_buf(),
        ..CertificateManagerConfig::for_core_db(state_dir)
    }
}
