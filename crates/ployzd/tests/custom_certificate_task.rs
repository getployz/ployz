use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
};
use ployz_core::ops::{CertOperationFailure, CertOperationState, OperationStatus, RouteHostname};
use ployz_test_support::ids::{cert_id, operation_id, route_hostname};
use ployzd::certificate::task::{
    CertificateRenewalOutcome, recover_unfinished_operations, run_once_at,
};
use ployzd::certificate::{
    AcmeIssueContext, AcmeIssuer, AcmeIssuerError, CertificateManager, CertificateManagerConfig,
    IssuedCertificate,
};
use ployzd::core_store::CoreStore;
use ployzd::intent::certificate_intent::CertificateIntentStore;
use ployzd::operations::log::{CertOperationSubmission, OperationRepository};
use rcgen::CertifiedKey;

#[tokio::test]
async fn failed_due_renewal_keeps_the_active_certificate_and_records_terminal_evidence() {
    let nats = ployz_test_support::nats::TestNats::start().await;
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
    let expected_gateway_ips = [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ];
    let original = manager
        .ensure(&hostname, &expected_gateway_ips)
        .await
        .expect("initial certificate");

    let outcome = run_once_at(&manager, &expected_gateway_ips, u64::MAX)
        .await
        .expect("renewal tick");
    let retained = manager
        .ensure(&hostname, &expected_gateway_ips)
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
async fn cancelled_waiter_leaves_cleanup_and_terminal_evidence_to_the_owned_task() {
    let nats = ployz_test_support::nats::TestNats::start().await;
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
    let waiter =
        tokio::spawn(async move { waiter_manager.ensure(&hostname, &loopback_ips()).await });
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
        CertificateIntentStore::reader(core_store)
            .challenges()
            .await
            .expect("challenge intent")
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_ensure_calls_share_one_certificate_issuance() {
    let nats = ployz_test_support::nats::TestNats::start().await;
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
    let expected_gateway_ips = loopback_ips();

    let (first, second) = tokio::join!(
        manager.ensure(&hostname, &expected_gateway_ips),
        manager.ensure(&hostname, &expected_gateway_ips)
    );

    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_eq!(issuer.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn expired_active_certificate_is_reissued() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let state_dir = tempfile::tempdir().expect("certificate state");
    let issuer = Arc::new(FakeAcmeIssuer::new([
        Ok(fixture_certificate("localhost")),
        Ok(fixture_certificate("localhost")),
    ]));
    let now = Arc::new(AtomicU64::new(1));
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
        .ensure(&hostname, &loopback_ips())
        .await
        .expect("initial certificate");
    now.store(u64::MAX, Ordering::Relaxed);

    manager
        .ensure(&hostname, &loopback_ips())
        .await
        .expect("replacement certificate");

    assert_eq!(issuer.calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn certificate_for_another_hostname_is_rejected_before_commit() {
    let nats = ployz_test_support::nats::TestNats::start().await;
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
        .ensure(&route_hostname("localhost"), &loopback_ips())
        .await
        .expect_err("mismatched certificate is rejected");

    assert!(matches!(
        error,
        ployzd::certificate::CertificateManagerError::AcmeValidation { .. }
    ));
    assert!(
        CertificateIntentStore::reader(core_store)
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
    let intent = CertificateIntentStore::new(core_store.clone(), state_dir.path().to_path_buf());
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
    assert!(matches!(
        recovered,
        OperationStatus::Cert {
            cert_id: status_cert_id,
            state: CertOperationState::Failed {
                failure: CertOperationFailure::AcmeValidationFailed {
                    cert_id: failure_cert_id,
                    retained_active_cert: None,
                    ..
                }
            },
            ..
        } if status_cert_id == &cert_id && failure_cert_id == &cert_id
    ));
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

fn test_config(state_dir: &std::path::Path) -> CertificateManagerConfig {
    CertificateManagerConfig {
        state_dir: state_dir.to_path_buf(),
        ..CertificateManagerConfig::for_core_db(state_dir)
    }
}
