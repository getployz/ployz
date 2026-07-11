use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ployz_core::cert::{LeaseBearerToken, ManagedLeaseAcquireRequest};
use ployz_lease_worker::{
    Clock, ClockError, LeaseWorkerRequest, LeaseWorkerResponse, STUB_LEASE_TTL_SECONDS,
    StubLeaseWorker,
};

#[derive(Clone)]
struct ManualClock {
    now: Arc<AtomicU64>,
}

impl ManualClock {
    fn new(now: u64) -> Self {
        Self {
            now: Arc::new(AtomicU64::new(now)),
        }
    }

    fn advance(&self, seconds: u64) {
        self.now.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_seconds(&self) -> Result<u64, ClockError> {
        Ok(self.now.load(Ordering::SeqCst))
    }
}

#[test]
fn stub_worker_issues_lease_and_downloads_bundle_with_bearer_token() {
    let mut worker = StubLeaseWorker::with_clock(ManualClock::new(1_700_000_000));
    let acquired = worker
        .handle(LeaseWorkerRequest::Acquire(ManagedLeaseAcquireRequest {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        }))
        .expect("lease acquired");

    let LeaseWorkerResponse::LeaseAcquired(acquired) = acquired else {
        panic!("expected lease acquisition response");
    };
    assert_eq!(
        acquired.lease.expires_at.unix_seconds() - acquired.lease.issued_at.unix_seconds(),
        STUB_LEASE_TTL_SECONDS
    );
    assert!(acquired.bundle.is_none());

    let pending = worker
        .handle(LeaseWorkerRequest::DownloadBundle {
            lease: acquired.lease.name.clone(),
            token: acquired.lease.token.clone(),
        })
        .expect("bundle pending");
    assert!(matches!(pending, LeaseWorkerResponse::BundlePending));

    let downloaded = worker
        .handle(LeaseWorkerRequest::DownloadBundle {
            lease: acquired.lease.name.clone(),
            token: acquired.lease.token.clone(),
        })
        .expect("bundle downloaded");
    let LeaseWorkerResponse::Bundle(bundle) = downloaded else {
        panic!("expected bundle response");
    };
    assert!(
        bundle
            .certificate_chain_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert!(
        bundle
            .private_key_pem
            .starts_with("-----BEGIN PRIVATE KEY-----")
    );
}

#[test]
fn stub_worker_acquisition_is_non_idempotent() {
    let mut worker = StubLeaseWorker::with_clock(ManualClock::new(1_700_000_000));
    let request = ManagedLeaseAcquireRequest {
        ipv4: vec!["203.0.113.8".parse().expect("IPv4")],
        ipv6: vec!["2001:db8::8".parse().expect("IPv6")],
    };
    let LeaseWorkerResponse::LeaseAcquired(first) = worker
        .handle(LeaseWorkerRequest::Acquire(request.clone()))
        .expect("first lease")
    else {
        panic!("expected first lease");
    };
    let LeaseWorkerResponse::LeaseAcquired(second) = worker
        .handle(LeaseWorkerRequest::Acquire(request))
        .expect("second lease")
    else {
        panic!("expected second lease");
    };

    assert_ne!(first.lease.name, second.lease.name);
}

#[test]
fn stub_worker_rejects_cookie_without_bearer_token() {
    let mut worker = StubLeaseWorker::with_clock(ManualClock::new(1_700_000_000));
    let acquired = worker
        .handle(LeaseWorkerRequest::Acquire(ManagedLeaseAcquireRequest {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        }))
        .expect("lease acquired");
    let LeaseWorkerResponse::LeaseAcquired(acquired) = acquired else {
        panic!("expected lease acquisition response");
    };
    let token = LeaseBearerToken::try_new("session_cookie_value").expect("valid token");

    let error = worker
        .handle(LeaseWorkerRequest::DownloadBundle {
            lease: acquired.lease.name,
            token,
        })
        .expect_err("cookie value is not lease bearer auth");

    assert_eq!(error.to_string(), "lease bearer token does not match");
}

#[test]
fn stub_worker_renewal_returns_fresh_bundle_for_existing_lease() {
    let clock = ManualClock::new(1_700_000_000);
    let mut worker = StubLeaseWorker::with_clock(clock.clone());
    let acquired = worker
        .handle(LeaseWorkerRequest::Acquire(ManagedLeaseAcquireRequest {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        }))
        .expect("lease acquired");
    let LeaseWorkerResponse::LeaseAcquired(acquired) = acquired else {
        panic!("expected lease acquisition response");
    };
    let pending = worker
        .handle(LeaseWorkerRequest::DownloadBundle {
            lease: acquired.lease.name.clone(),
            token: acquired.lease.token.clone(),
        })
        .expect("bundle pending");
    assert!(matches!(pending, LeaseWorkerResponse::BundlePending));
    let downloaded = worker
        .handle(LeaseWorkerRequest::DownloadBundle {
            lease: acquired.lease.name.clone(),
            token: acquired.lease.token.clone(),
        })
        .expect("bundle downloaded");
    let LeaseWorkerResponse::Bundle(bundle) = downloaded else {
        panic!("expected bundle response");
    };
    clock.advance(60);

    let renewed = worker
        .handle(LeaseWorkerRequest::Renew {
            lease: acquired.lease.name.clone(),
            token: acquired.lease.token.clone(),
            request: ManagedLeaseAcquireRequest {
                ipv4: Vec::new(),
                ipv6: Vec::new(),
            },
        })
        .expect("lease renewed");

    let LeaseWorkerResponse::LeaseRenewed(renewed) = renewed else {
        panic!("expected renewal response");
    };
    let renewed_bundle = renewed.bundle.expect("ready renewal returns a bundle");
    assert_ne!(renewed_bundle.digest, bundle.digest);
    assert!(renewed.lease.issued_at > acquired.lease.issued_at);
    assert!(renewed.lease.expires_at > acquired.lease.expires_at);
}
