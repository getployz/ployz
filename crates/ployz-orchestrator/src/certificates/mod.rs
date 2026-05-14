use std::time::Duration;

const CERT_VALIDITY_FALLBACK_SECS: u64 = 90 * 24 * 60 * 60;
// Finalization runs in the background, so this can cover unusually slow store
// propagation before reachable peers must observe the HTTP-01 record.
const STUCK_ISSUING_MAX_AGE_SECS: u64 = 24 * 60 * 60;
pub const HTTP01_CHALLENGE_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(2 * 60);
pub const HTTP01_CHALLENGE_VISIBILITY_POLL: Duration = Duration::from_millis(100);

mod finalization;
mod orders;
mod readiness;
mod renewal;

pub use finalization::{
    finalize_due_certificates, renewal_threshold, spawn_certificate_finalization,
    spawn_certificate_finalization_with_coordination,
    spawn_certificate_finalization_with_readiness,
};
pub use orders::start_pending_orders;
pub use readiness::{LocalHttp01ChallengeReadiness, wait_for_http01_challenge_visible};
pub use renewal::{CertificateRenewalTask, process_renewal_job};

#[cfg(test)]
mod tests {
    use super::finalization::finalize_one;
    use super::orders::start_one;
    use super::*;
    use async_trait::async_trait;
    use ployz_cert_api::{
        AcmeIssuer, CHALLENGE_TTL_SECS, DEFAULT_ACME_DIRECTORY_URL, IssuanceAcquisition,
        IssuanceCoordinator, IssuanceHold, IssuedCertificate, NoopIssuanceCoordinator,
        StartedOrder, account_id_for_issuer_url,
    };
    use ployz_error::{CertificateError, Error, Result};
    use ployz_model::{
        AcmeChallengeRecord, CertificateLifecycle, CertificateRecord, CertificateState,
        CertificateVersion,
    };
    use ployz_store_api::{CertificateStore, StoreDriver};
    use ployz_store_memory::StoreDriverMemoryExt as _;
    use ployz_time::now_unix_secs;
    use std::sync::Mutex;
    use std::time::Duration;

    fn set_issuing(record: &mut CertificateRecord, order_url: &str) {
        record.lifecycle = CertificateLifecycle::Issuing {
            order_url: order_url.into(),
            active_version_id: None,
            last_error: None,
        };
    }

    fn set_issuing_with_active(record: &mut CertificateRecord, order_url: &str, version_id: &str) {
        record.lifecycle = CertificateLifecycle::Issuing {
            order_url: order_url.into(),
            active_version_id: Some(version_id.into()),
            last_error: None,
        };
    }

    fn set_active(record: &mut CertificateRecord, version_id: &str, next_renewal_at: Option<u64>) {
        record.lifecycle = CertificateLifecycle::Active {
            active_version_id: version_id.into(),
            next_renewal_at,
        };
    }

    fn set_renewal_due(
        record: &mut CertificateRecord,
        version_id: &str,
        next_renewal_at: Option<u64>,
    ) {
        record.lifecycle = CertificateLifecycle::RenewalDue {
            active_version_id: version_id.into(),
            next_renewal_at,
        };
    }

    fn set_failed(record: &mut CertificateRecord, error: &str, active_version_id: Option<&str>) {
        record.lifecycle = CertificateLifecycle::Failed {
            last_error: error.into(),
            active_version_id: active_version_id.map(String::from),
        };
    }

    struct FakeIssuer {
        start_result: Mutex<Option<Result<StartedOrder>>>,
        finalize_result: Mutex<Option<Result<IssuedCertificate>>>,
    }

    enum FinalizeMutation {
        Activate {
            active_version_id: String,
            fullchain_pem: String,
            private_key_pem: String,
        },
        ReplaceOrder {
            order_url: String,
        },
    }

    struct MutatingFinalizeIssuer {
        mutation: FinalizeMutation,
        result: Mutex<Option<Result<IssuedCertificate>>>,
    }

    impl MutatingFinalizeIssuer {
        fn new(mutation: FinalizeMutation, result: Result<IssuedCertificate>) -> Self {
            Self {
                mutation,
                result: Mutex::new(Some(result)),
            }
        }
    }

    impl FakeIssuer {
        fn new(
            start_result: Result<StartedOrder>,
            finalize_result: Result<IssuedCertificate>,
        ) -> Self {
            Self {
                start_result: Mutex::new(Some(start_result)),
                finalize_result: Mutex::new(Some(finalize_result)),
            }
        }

        fn start_only(start_result: Result<StartedOrder>) -> Self {
            Self {
                start_result: Mutex::new(Some(start_result)),
                finalize_result: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AcmeIssuer for FakeIssuer {
        async fn start_order(&self, _store: &StoreDriver, _hostname: &str) -> Result<StartedOrder> {
            self.start_result
                .lock()
                .expect("start_result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("fake_start_order", "exhausted")))
        }

        async fn finalize_order(
            &self,
            _store: &StoreDriver,
            _hostname: &str,
            _order_url: &str,
        ) -> Result<IssuedCertificate> {
            self.finalize_result
                .lock()
                .expect("finalize_result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("fake_finalize_order", "exhausted")))
        }
    }

    #[async_trait]
    impl AcmeIssuer for MutatingFinalizeIssuer {
        async fn start_order(&self, _store: &StoreDriver, _hostname: &str) -> Result<StartedOrder> {
            Err(Error::operation("fake_start_order", "unused"))
        }

        async fn finalize_order(
            &self,
            store: &StoreDriver,
            _hostname: &str,
            _order_url: &str,
        ) -> Result<IssuedCertificate> {
            let mut current = store
                .get_certificate("example.com")
                .await?
                .ok_or_else(|| Error::operation("fake_finalize_order", "missing cert"))?;
            match &self.mutation {
                FinalizeMutation::Activate {
                    active_version_id,
                    fullchain_pem,
                    private_key_pem,
                } => {
                    current.lifecycle = CertificateLifecycle::Active {
                        active_version_id: active_version_id.clone(),
                        next_renewal_at: None,
                    };
                    current.versions.push(CertificateVersion {
                        version_id: active_version_id.clone(),
                        fullchain_pem: fullchain_pem.clone(),
                        private_key_pem: private_key_pem.clone(),
                        not_before: Some(1),
                        not_after: Some(2),
                        issued_at: 1,
                    });
                }
                FinalizeMutation::ReplaceOrder { order_url } => {
                    current.lifecycle = CertificateLifecycle::Issuing {
                        order_url: order_url.clone(),
                        active_version_id: current.active_version_id().map(ToString::to_string),
                        last_error: current.last_error().map(ToString::to_string),
                    };
                    current.updated_at = now_unix_secs();
                }
            }
            store.upsert_certificate(&current).await?;
            self.result
                .lock()
                .expect("finalize_result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("fake_finalize_order", "exhausted")))
        }
    }

    // -------------------------------------------------------------------
    // start_one — multi-daemon order-creation safety
    //
    // These tests pin the contract that protects against duplicate ACME
    // orders in a clustered deployment:
    //
    //   1. The cluster lock must cover both the external `start_order`
    //      side effect AND the record update — otherwise a peer can read
    //      the still-Pending record in the gap, acquire
    //      the (released) lock, and create a duplicate order.
    //
    //   2. After acquiring the lock, the record must be re-read, because the
    //      snapshot handed in by `start_pending_orders` may already be stale.
    //      A record that is no longer Pending/Failed/RenewalDue must not
    //      trigger a new ACME order.
    // -------------------------------------------------------------------

    /// Issuer that aborts the test if `start_order` is invoked. Used to
    /// assert "we never asked ACME for a new order in this scenario."
    struct PanickingIssuer;

    #[async_trait]
    impl AcmeIssuer for PanickingIssuer {
        async fn start_order(&self, _: &StoreDriver, _: &str) -> Result<StartedOrder> {
            panic!("start_order should not be invoked");
        }
        async fn finalize_order(
            &self,
            _: &StoreDriver,
            _: &str,
            _: &str,
        ) -> Result<IssuedCertificate> {
            Err(Error::operation("panicking_issuer", "unused"))
        }
    }

    /// Coordinator that captures the certificate record's state at the moment
    /// `IssuanceHold::release` runs. Lets the test assert that the lock is
    /// still held when the record was upserted with `Issuing` + `order_url`.
    struct CaptureOnReleaseCoordinator {
        store: StoreDriver,
        captured: std::sync::Arc<Mutex<Option<CertificateRecord>>>,
    }

    #[async_trait]
    impl IssuanceCoordinator for CaptureOnReleaseCoordinator {
        async fn try_acquire(&self, hostname: &str) -> IssuanceAcquisition {
            let store = self.store.clone();
            let captured = std::sync::Arc::clone(&self.captured);
            let hostname = hostname.to_string();
            IssuanceAcquisition::Allowed(IssuanceHold::new(move || async move {
                if let Ok(Some(record)) = store.get_certificate(&hostname).await {
                    *captured.lock().expect("captured lock") = Some(record);
                }
            }))
        }
    }

    #[tokio::test]
    async fn start_one_skips_when_record_already_issuing_after_lock_acquire() {
        // Simulate: peer A held the lock, ran start_order, persisted Issuing
        // with an order_url, released. Peer B's `start_pending_orders` had
        // already read the record as Pending before A's write replicated, so
        // it hands a stale snapshot to start_one. The lock-bound re-read
        // must catch this and skip — otherwise B would mint a duplicate
        // ACME order for the same hostname.
        let store = StoreDriver::memory();
        let mut already_issuing = pending_record("example.com");
        set_issuing(&mut already_issuing, "https://acme/orders/41");
        store
            .upsert_certificate(&already_issuing)
            .await
            .expect("already-issuing cert should persist");

        // Stale snapshot of the same record, as start_pending_orders would have
        // observed it before peer A's write reached this daemon.
        let stale = pending_record("example.com");

        let warning = start_one(&store, &PanickingIssuer, &NoopIssuanceCoordinator, stale)
            .await
            .expect("stale start should not fail coordination");
        assert!(warning.is_none(), "stale start should be skipped silently");

        // Record is unchanged: A's order_url and Issuing state are intact.
        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://acme/orders/41"));
    }

    #[tokio::test]
    async fn start_one_holds_lock_until_after_upsert() {
        // The cluster lock must cover the record write, not just `start_order`.
        // We assert this by capturing the record's state at the exact moment
        // `IssuanceHold::release` runs: if the lock covers the upsert, the
        // captured record already has Issuing + the new order_url.
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        let captured = std::sync::Arc::new(Mutex::new(None));
        let coordinator = CaptureOnReleaseCoordinator {
            store: store.clone(),
            captured: std::sync::Arc::clone(&captured),
        };

        let stale = store
            .get_certificate("example.com")
            .await
            .expect("read snapshot")
            .expect("snapshot record");

        let warning = start_one(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &coordinator,
            stale,
        )
        .await
        .expect("happy-path start should not fail coordination");
        assert!(warning.is_none(), "happy-path start should not warn");

        let snapshot_at_release = captured
            .lock()
            .expect("captured lock")
            .clone()
            .expect("release should have captured the record");
        // If the lock had been released before the upsert, this would still
        // be Pending with no order_url.
        assert_eq!(snapshot_at_release.state(), CertificateState::Issuing);
        assert_eq!(
            snapshot_at_release.order_url(),
            Some("https://acme/orders/42")
        );
    }

    #[tokio::test]
    async fn start_one_prunes_stale_challenge_records_for_same_hostname() {
        // Failed-then-retry scenario: a previous order left stale challenge
        // records behind because finalize_order's success path is the only
        // place that deletes them. The next `start_one` must prune those
        // before minting a new order — otherwise `acme_challenges` grows
        // unbounded across repeated failures, replicates the leak across
        // the cluster, and bloats every gateway snapshot rebuild.
        let store = StoreDriver::memory();
        let mut failed = pending_record("example.com");
        set_failed(&mut failed, "previous order failed", None);
        store
            .upsert_certificate(&failed)
            .await
            .expect("failed cert should persist");

        // Two stale tokens for the failing hostname plus an unrelated
        // hostname's token that must NOT be pruned.
        for (hostname, token) in [
            ("example.com", "stale-tok-A"),
            ("example.com", "stale-tok-B"),
            ("other.example.com", "keep-tok"),
        ] {
            store
                .upsert_acme_challenge(&AcmeChallengeRecord {
                    hostname: hostname.into(),
                    token: token.into(),
                    key_authorization: format!("{token}.keyauth"),
                    expires_at: now_unix_secs() + 60,
                    created_at: now_unix_secs(),
                })
                .await
                .expect("challenge upsert should persist");
        }

        let warning = start_one(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &NoopIssuanceCoordinator,
            failed,
        )
        .await
        .expect("happy retry should not fail coordination");
        assert!(warning.is_none(), "happy retry should not warn");

        let remaining = store
            .list_acme_challenges()
            .await
            .expect("list should work");
        // FakeIssuer::start_only doesn't write any new challenge records; we
        // only assert pruning here, so the surviving record is the unrelated
        // hostname's challenge.
        assert_eq!(remaining.len(), 1, "stale records should be pruned");
        assert_eq!(remaining[0].hostname, "other.example.com");
        assert_eq!(remaining[0].token, "keep-tok");
    }

    #[tokio::test]
    async fn start_pending_transitions_to_issuing_with_order_url() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        let warnings = start_pending_orders(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &NoopIssuanceCoordinator,
            &["example.com".into()],
        )
        .await;
        assert!(warnings.is_empty(), "healthy start should not warn");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://acme/orders/42"));
        assert!(record.last_error().is_none());
    }

    #[tokio::test]
    async fn start_pending_surfaces_rate_limit_as_warning() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        let warnings = start_pending_orders(
            &store,
            &FakeIssuer::start_only(Err(Error::operation(
                "new_order",
                "urn:ietf:params:acme:error:rateLimited: too many",
            ))),
            &NoopIssuanceCoordinator,
            &["example.com".into()],
        )
        .await;
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("example.com"));
        assert!(warnings[0].contains("rateLimited"));

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Failed);
        assert!(record.last_error().unwrap().contains("rateLimited"));
        assert!(record.order_url().is_none());
    }

    #[tokio::test]
    async fn start_pending_only_touches_requested_hostnames() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("target cert should persist");
        store
            .upsert_certificate(&pending_record("unrelated.example.com"))
            .await
            .expect("unrelated cert should persist");

        let warnings = start_pending_orders(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &NoopIssuanceCoordinator,
            &["example.com".into()],
        )
        .await;
        assert!(warnings.is_empty(), "healthy start should not warn");

        let target = store
            .get_certificate("example.com")
            .await
            .expect("target cert lookup should work")
            .expect("target cert should exist");
        let unrelated = store
            .get_certificate("unrelated.example.com")
            .await
            .expect("unrelated cert lookup should work")
            .expect("unrelated cert should exist");
        assert_eq!(target.state(), CertificateState::Issuing);
        assert_eq!(unrelated.state(), CertificateState::Pending);
        assert!(unrelated.order_url().is_none());
    }

    #[tokio::test]
    async fn finalize_due_writes_active_certificate() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_due_certificates(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("unused", "start not called")),
                Ok(IssuedCertificate {
                    fullchain_pem: "fullchain".into(),
                    private_key_pem: "key".into(),
                }),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("finalization should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Active);
        assert!(record.active_version_id().is_some());
        assert_eq!(record.versions.len(), 1);
        assert_eq!(record.versions[0].fullchain_pem, "fullchain");
        assert!(record.order_url().is_none());
    }

    #[tokio::test]
    async fn finalize_failure_keeps_previous_active_version() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing_with_active(&mut record, "https://acme/orders/42", "old");
        record.versions.push(CertificateVersion {
            version_id: "old".into(),
            fullchain_pem: "old-chain".into(),
            private_key_pem: "old-key".into(),
            not_before: Some(1),
            not_after: Some(2),
            issued_at: 1,
        });
        store
            .upsert_certificate(&record)
            .await
            .expect("renewal cert should persist");

        finalize_due_certificates(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("unused", "start not called")),
                Err(Error::operation("fake_acme", "failed")),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("finalization errors are recorded per certificate");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Failed);
        assert_eq!(record.active_version_id(), Some("old"));
        assert_eq!(record.versions.len(), 1);
        assert!(record.order_url().is_none());
    }

    #[tokio::test]
    async fn stale_finalize_failure_does_not_overwrite_active_certificate() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_one(
            &store,
            &MutatingFinalizeIssuer::new(
                FinalizeMutation::Activate {
                    active_version_id: "new".into(),
                    fullchain_pem: "new-chain".into(),
                    private_key_pem: "new-key".into(),
                },
                Err(Error::operation("fake_acme", "late failure")),
            ),
            &NoopIssuanceCoordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("stale finalization should be skipped");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Active);
        assert_eq!(record.active_version_id(), Some("new"));
        assert_eq!(record.versions.len(), 1);
        assert!(record.order_url().is_none());
        assert!(record.last_error().is_none());
    }

    #[tokio::test]
    async fn stale_finalize_success_does_not_overwrite_new_order() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_one(
            &store,
            &MutatingFinalizeIssuer::new(
                FinalizeMutation::ReplaceOrder {
                    order_url: "https://acme/orders/43".into(),
                },
                Ok(IssuedCertificate {
                    fullchain_pem: "stale-chain".into(),
                    private_key_pem: "stale-key".into(),
                }),
            ),
            &NoopIssuanceCoordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("stale finalization should be skipped");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://acme/orders/43"));
        assert!(record.active_version_id().is_none());
        assert!(record.versions.is_empty());
    }

    /// Issuer that records each `finalize_order` call. Lets the pre-call
    /// guard test assert the ACME side-effect path was not entered when
    /// `finalize_one` is handed a record that's already been rotated to a
    /// newer order.
    struct RecordingFinalizeIssuer {
        finalize_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl RecordingFinalizeIssuer {
        fn new() -> Self {
            Self {
                finalize_calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn finalize_call_count(&self) -> usize {
            self.finalize_calls
                .load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl AcmeIssuer for RecordingFinalizeIssuer {
        async fn start_order(&self, _: &StoreDriver, _: &str) -> Result<StartedOrder> {
            Err(Error::operation("recording_issuer", "start_order unused"))
        }

        async fn finalize_order(
            &self,
            _: &StoreDriver,
            _: &str,
            _: &str,
        ) -> Result<IssuedCertificate> {
            self.finalize_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(IssuedCertificate {
                fullchain_pem: "should-not-be-used".into(),
                private_key_pem: "should-not-be-used".into(),
            })
        }
    }

    #[tokio::test]
    async fn finalize_one_skips_acme_when_stored_record_already_advanced_past_order() {
        // A peer's `start_one` has already rotated the record to a newer order
        // (order/43). This daemon still holds a stale snapshot
        // that points at order/42. `finalize_one` must short-circuit before
        // calling the ACME finalize step — running it would delete or
        // disturb challenge state for the in-flight order/43 (the original
        // bug: stale finalizers ran ACME side effects unconditionally).
        let store = StoreDriver::memory();
        let mut current = pending_record("example.com");
        set_issuing(&mut current, "https://acme/orders/43");
        store
            .upsert_certificate(&current)
            .await
            .expect("issuing cert should persist");

        let stale_snapshot = {
            let mut record = pending_record("example.com");
            set_issuing(&mut record, "https://acme/orders/42");
            record
        };

        let issuer = RecordingFinalizeIssuer::new();
        finalize_one(
            &store,
            &issuer,
            &NoopIssuanceCoordinator,
            stale_snapshot,
            "https://acme/orders/42",
        )
        .await
        .expect("stale finalization should be skipped without error");

        assert_eq!(
            issuer.finalize_call_count(),
            0,
            "ACME finalize_order must not run when the stored record already points at a newer order"
        );

        // The newer order's record is untouched.
        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://acme/orders/43"));
    }

    // -------------------------------------------------------------------
    // finalize_one — multi-daemon order-finalization safety
    //
    // The pre-fix race: every daemon independently finalizes the same
    // Issuing record. Exactly one wins `finalize()` at LE; the losers' fast
    // `Failed` writes beat the winner's slow `poll_certificate` and the
    // winner's post-check then sees `Failed` and drops the issued cert.
    // On a 300-node cluster this fires every cycle and burns LE's
    // duplicate-cert rate limit.
    //
    // The fix: hold the same hostname-scoped cluster lock `start_one`
    // uses for the entire ACME flow + persistence. These tests pin that
    // contract.
    // -------------------------------------------------------------------

    /// Coordinator that always vetoes. Models a peer already holding the
    /// hostname lock.
    struct AlwaysVetoCoordinator;

    #[async_trait]
    impl IssuanceCoordinator for AlwaysVetoCoordinator {
        async fn try_acquire(&self, _hostname: &str) -> IssuanceAcquisition {
            IssuanceAcquisition::VetoedByPeer("peer holds lock".into())
        }
    }

    struct FailingCoordinator;

    #[async_trait]
    impl IssuanceCoordinator for FailingCoordinator {
        async fn try_acquire(&self, _hostname: &str) -> IssuanceAcquisition {
            IssuanceAcquisition::CoordinationFailed("nats lock backend unavailable".into())
        }
    }

    #[tokio::test]
    async fn start_pending_surfaces_coordination_failure_as_warning() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        let warnings = start_pending_orders(
            &store,
            &PanickingIssuer,
            &FailingCoordinator,
            &["example.com".into()],
        )
        .await;

        let [warning] = warnings.as_slice() else {
            panic!("expected one coordination warning, got {warnings:?}");
        };
        assert!(warning.contains("during issuance"));
        assert!(warning.contains("nats lock backend unavailable"));
    }

    #[tokio::test]
    async fn process_renewal_job_fails_on_coordination_backend_error() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_renewal_due(&mut record, "v1", None);
        store
            .upsert_certificate(&record)
            .await
            .expect("renewal due cert should persist");

        let error =
            process_renewal_job(&store, &PanickingIssuer, &FailingCoordinator, "example.com")
                .await
                .expect_err("renewal job should fail on lock backend error");

        assert!(matches!(
            &error,
            Error::Certificate(CertificateError::CertificateLockAcquireFailed {
                phase: "issuance",
                ..
            })
        ));
        assert!(error.to_string().contains("nats lock backend unavailable"));
    }

    #[tokio::test]
    async fn finalize_one_skips_acme_when_coordinator_vetoes() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        let issuer = RecordingFinalizeIssuer::new();
        finalize_one(
            &store,
            &issuer,
            &AlwaysVetoCoordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("vetoed finalize should be a no-op, not an error");

        assert_eq!(
            issuer.finalize_call_count(),
            0,
            "ACME finalize_order must not run when a peer holds the lock"
        );

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://acme/orders/42"));
        assert!(record.last_error().is_none());
    }

    #[tokio::test]
    async fn finalize_one_fails_on_coordination_backend_error() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        let issuer = RecordingFinalizeIssuer::new();
        let error = finalize_one(
            &store,
            &issuer,
            &FailingCoordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect_err("lock backend failure should fail finalization");

        assert!(matches!(
            &error,
            Error::Certificate(CertificateError::CertificateLockAcquireFailed {
                phase: "finalization",
                ..
            })
        ));
        assert!(error.to_string().contains("nats lock backend unavailable"));
        assert_eq!(issuer.finalize_call_count(), 0);
    }

    #[tokio::test]
    async fn finalize_one_holds_lock_until_after_persist() {
        // Mirrors `start_one_holds_lock_until_after_upsert`. The lock must
        // cover the record write so a peer can't read the still-Issuing record
        // between `finalize_order` returning and the persist landing,
        // grab the (released) lock, and start a duplicate fresh order.
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        let captured = std::sync::Arc::new(Mutex::new(None));
        let coordinator = CaptureOnReleaseCoordinator {
            store: store.clone(),
            captured: std::sync::Arc::clone(&captured),
        };
        let issuer = FakeIssuer::new(
            Err(Error::operation("unused", "start not called")),
            Ok(IssuedCertificate {
                fullchain_pem: "fullchain".into(),
                private_key_pem: "key".into(),
            }),
        );

        finalize_one(
            &store,
            &issuer,
            &coordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("finalization should succeed");

        let record_at_release = captured
            .lock()
            .expect("captured lock")
            .clone()
            .expect("release callback should observe a record");
        assert_eq!(record_at_release.state(), CertificateState::Active);
        assert!(record_at_release.active_version_id().is_some());
        assert!(record_at_release.order_url().is_none());
        let [version] = record_at_release.versions.as_slice() else {
            panic!(
                "expected exactly one issued version, got {:?}",
                record_at_release.versions
            );
        };
        assert_eq!(version.fullchain_pem, "fullchain");
    }

    /// Coordinator backed by a `tokio::sync::Mutex`. Concurrent acquires
    /// return `VetoedByPeer` synchronously rather than waiting, mirroring
    /// the production reservation semantics where peers either hold the
    /// reservation or get a synchronous deny.
    struct SerializingCoordinator {
        held: std::sync::Arc<tokio::sync::Mutex<()>>,
    }

    impl Default for SerializingCoordinator {
        fn default() -> Self {
            Self {
                held: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            }
        }
    }

    #[async_trait]
    impl IssuanceCoordinator for SerializingCoordinator {
        async fn try_acquire(&self, _hostname: &str) -> IssuanceAcquisition {
            match self.held.clone().try_lock_owned() {
                Ok(guard) => IssuanceAcquisition::Allowed(IssuanceHold::new(move || async move {
                    drop(guard);
                })),
                Err(_) => IssuanceAcquisition::VetoedByPeer("peer holds lock".into()),
            }
        }
    }

    /// Issuer whose `finalize_order` sleeps before returning, modelling
    /// LE's poll_ready + finalize + poll_certificate round-trip. Forces
    /// concurrent finalizers to overlap when running under a multi-thread
    /// runtime so the lock's serialization is actually exercised.
    struct SlowFinalizeIssuer {
        delay: Duration,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        result: std::sync::Mutex<Option<Result<IssuedCertificate>>>,
    }

    impl SlowFinalizeIssuer {
        fn new(delay: Duration, result: Result<IssuedCertificate>) -> Self {
            Self {
                delay,
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                result: std::sync::Mutex::new(Some(result)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl AcmeIssuer for SlowFinalizeIssuer {
        async fn start_order(&self, _: &StoreDriver, _: &str) -> Result<StartedOrder> {
            Err(Error::operation("slow_issuer", "start unused"))
        }

        async fn finalize_order(
            &self,
            _: &StoreDriver,
            _: &str,
            _: &str,
        ) -> Result<IssuedCertificate> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.result
                .lock()
                .expect("slow_issuer result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("slow_issuer", "exhausted")))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_finalize_one_calls_serialize_through_lock() {
        // Eight concurrent finalize_one tasks against the same Issuing record.
        // Exactly one acquires the cluster lock and runs ACME; the seven
        // losers see VetoedByPeer and return early without touching the
        // record. The issued cert lands on the record — never `Failed`. This is
        // the test that would have caught the original bug: pre-fix, the
        // losers would have raced past the pre-check, run ACME, hit the
        // already-finalized order at LE, and written `Failed` before the
        // winner's `poll_certificate` returned.
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        let coordinator = std::sync::Arc::new(SerializingCoordinator::default());
        let issuer = std::sync::Arc::new(SlowFinalizeIssuer::new(
            Duration::from_millis(150),
            Ok(IssuedCertificate {
                fullchain_pem: "winner-chain".into(),
                private_key_pem: "winner-key".into(),
            }),
        ));

        let runs: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let coord = std::sync::Arc::clone(&coordinator);
                let issuer = std::sync::Arc::clone(&issuer);
                let record = record.clone();
                tokio::spawn(async move {
                    finalize_one(
                        &store,
                        issuer.as_ref(),
                        coord.as_ref(),
                        record,
                        "https://acme/orders/42",
                    )
                    .await
                })
            })
            .collect();

        for run in runs {
            run.await
                .expect("task join")
                .expect("finalize_one should not error");
        }

        assert_eq!(
            issuer.calls(),
            1,
            "exactly one daemon should have run ACME finalize"
        );

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(
            record.state(),
            CertificateState::Active,
            "issued cert must land on the record, not Failed"
        );
        let [version] = record.versions.as_slice() else {
            panic!(
                "expected exactly one issued version, got {:?}",
                record.versions
            );
        };
        assert_eq!(version.fullchain_pem, "winner-chain");
        assert!(record.last_error().is_none());
        assert!(record.order_url().is_none());
    }

    #[tokio::test]
    async fn challenge_visibility_failure_keeps_order_issuing_for_retry() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/42");
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_due_certificates(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("unused", "start not called")),
                Err(Error::operation(
                    "acme_challenge_visibility",
                    "peer did not see challenge yet",
                )),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("retryable visibility errors are recorded per certificate");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://acme/orders/42"));
        assert!(
            record
                .last_error()
                .is_some_and(|error| error.contains("peer did not see challenge yet"))
        );
    }

    #[tokio::test]
    async fn http01_visibility_wait_observes_store_challenge() {
        let store = StoreDriver::memory();
        store
            .upsert_acme_challenge(&AcmeChallengeRecord {
                hostname: "example.com".into(),
                token: "token-1".into(),
                key_authorization: "key-auth".into(),
                expires_at: now_unix_secs() + CHALLENGE_TTL_SECS,
                created_at: now_unix_secs(),
            })
            .await
            .expect("challenge should persist");

        wait_for_http01_challenge_visible(&store, "example.com", "token-1")
            .await
            .expect("stored challenge should be visible");
    }

    #[test]
    fn renewal_threshold_is_two_thirds_of_lifetime() {
        // 90-day cert → renew with 30 days remaining
        let ninety_days: u64 = 90 * 24 * 60 * 60;
        let threshold =
            renewal_threshold(Some(0), Some(ninety_days)).expect("threshold computable");
        assert_eq!(threshold, ninety_days * 2 / 3);
        // 6-day cert → renew with 2 days remaining
        let six_days: u64 = 6 * 24 * 60 * 60;
        let threshold = renewal_threshold(Some(0), Some(six_days)).expect("threshold computable");
        assert_eq!(threshold, six_days * 2 / 3);
    }

    #[test]
    fn account_id_tracks_issuer_url() {
        let issuer_url = "https://acme-staging-v02.api.letsencrypt.org/directory";
        assert_eq!(account_id_for_issuer_url(issuer_url), issuer_url);
    }

    #[tokio::test]
    async fn renewal_job_flips_active_past_threshold_to_renewal_due() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut record = pending_record("example.com");
        set_active(&mut record, "v1", Some(now.saturating_sub(10)));
        store
            .upsert_certificate(&record)
            .await
            .expect("active cert should persist");

        process_renewal_job(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("fake_start_order", "no work expected")),
                Err(Error::operation("fake_finalize_order", "no work expected")),
            ),
            &NoopIssuanceCoordinator,
            "example.com",
        )
        .await
        .expect("renewal job should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        // RenewalDue was picked up by start_pending_orders in the same job.
        // FakeIssuer returned Err → state is Failed, last_error captured.
        assert_eq!(record.state(), CertificateState::Failed);
        assert!(record.last_error().is_some());
    }

    #[tokio::test]
    async fn renewal_job_resets_stuck_issuing_to_pending() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut record = pending_record("example.com");
        set_issuing(&mut record, "https://acme/orders/stale");
        record.updated_at = now.saturating_sub(STUCK_ISSUING_MAX_AGE_SECS + 1);
        store
            .upsert_certificate(&record)
            .await
            .expect("stuck cert should persist");

        process_renewal_job(
            &store,
            &FakeIssuer::new(
                Ok(StartedOrder {
                    order_url: "https://acme/orders/new".into(),
                }),
                Err(Error::operation("fake_finalize_order", "no work expected")),
            ),
            &NoopIssuanceCoordinator,
            "example.com",
        )
        .await
        .expect("renewal job should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        // Stuck Issuing flipped to Pending, then start_pending_orders opened
        // a new order and moved it to Issuing with the fresh URL.
        assert_eq!(record.state(), CertificateState::Issuing);
        assert_eq!(record.order_url(), Some("https://acme/orders/new"));
    }

    #[tokio::test]
    async fn renewal_job_skips_active_before_threshold() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut record = pending_record("example.com");
        set_active(&mut record, "v1", Some(now.saturating_add(3600)));
        store
            .upsert_certificate(&record)
            .await
            .expect("active cert should persist");

        process_renewal_job(
            &store,
            &FakeIssuer::start_only(Err(Error::operation(
                "fake_start_order",
                "should not be called",
            ))),
            &NoopIssuanceCoordinator,
            "example.com",
        )
        .await
        .expect("renewal job should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state(), CertificateState::Active);
        assert!(record.last_error().is_none());
    }

    #[tokio::test]
    async fn renewal_job_processes_one_due_hostname() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut due = pending_record("due.example.com");
        set_active(&mut due, "v1", Some(now.saturating_sub(1)));
        let mut other = pending_record("other.example.com");
        set_active(&mut other, "v1", Some(now.saturating_sub(1)));
        store
            .upsert_certificate(&due)
            .await
            .expect("due cert should persist");
        store
            .upsert_certificate(&other)
            .await
            .expect("other cert should persist");

        process_renewal_job(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/due".into(),
            })),
            &NoopIssuanceCoordinator,
            "due.example.com",
        )
        .await
        .expect("renewal job should run");

        let due = store
            .get_certificate("due.example.com")
            .await
            .expect("cert lookup should work")
            .expect("due cert should exist");
        let other = store
            .get_certificate("other.example.com")
            .await
            .expect("cert lookup should work")
            .expect("other cert should exist");
        assert_eq!(due.state(), CertificateState::Issuing);
        assert_eq!(due.order_url(), Some("https://acme/orders/due"));
        assert_eq!(other.state(), CertificateState::Active);
        assert!(other.order_url().is_none());
    }

    #[tokio::test]
    async fn renewal_job_skips_missing_hostname() {
        let store = StoreDriver::memory();

        process_renewal_job(
            &store,
            &FakeIssuer::start_only(Err(Error::operation(
                "fake_start_order",
                "should not be called",
            ))),
            &NoopIssuanceCoordinator,
            "missing.example.com",
        )
        .await
        .expect("missing record should be a no-op");
    }

    #[tokio::test(start_paused = true)]
    async fn http01_challenge_visibility_returns_immediately_when_already_present() {
        let store = StoreDriver::memory();
        store
            .upsert_acme_challenge(&AcmeChallengeRecord {
                hostname: "example.com".into(),
                token: "tok-A".into(),
                key_authorization: "tok-A.keyauth".into(),
                expires_at: now_unix_secs() + CHALLENGE_TTL_SECS,
                created_at: now_unix_secs(),
            })
            .await
            .expect("seed challenge");

        let started = tokio::time::Instant::now();
        wait_for_http01_challenge_visible(&store, "example.com", "tok-A")
            .await
            .expect("already-visible challenge should return Ok");
        // The function only sleeps after a miss, so the hit case should not
        // advance virtual time at all.
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn http01_challenge_visibility_succeeds_after_late_write() {
        let store = StoreDriver::memory();
        let writer_store = store.clone();
        let writer = tokio::spawn(async move {
            // Make the visibility loop poll a few times before the record appears.
            tokio::time::sleep(HTTP01_CHALLENGE_VISIBILITY_POLL * 50).await;
            writer_store
                .upsert_acme_challenge(&AcmeChallengeRecord {
                    hostname: "example.com".into(),
                    token: "tok-B".into(),
                    key_authorization: "tok-B.keyauth".into(),
                    expires_at: now_unix_secs() + CHALLENGE_TTL_SECS,
                    created_at: now_unix_secs(),
                })
                .await
                .expect("delayed challenge upsert");
        });

        wait_for_http01_challenge_visible(&store, "example.com", "tok-B")
            .await
            .expect("late-written challenge should be observed");
        writer.await.expect("writer should not panic");
    }

    #[tokio::test(start_paused = true)]
    async fn http01_challenge_visibility_times_out() {
        let store = StoreDriver::memory();

        let error = wait_for_http01_challenge_visible(&store, "example.com", "tok-missing")
            .await
            .expect_err("missing challenge should time out");

        assert!(
            matches!(
                &error,
                Error::Certificate(CertificateError::Http01LocalChallengeNotVisible { .. })
            ),
            "expected HTTP-01 visibility error, got: {error:?}"
        );
        assert!(
            error.to_string().contains("example.com"),
            "expected hostname in error, got: {error}"
        );
    }

    fn pending_record(hostname: &str) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: DEFAULT_ACME_DIRECTORY_URL.into(),
            account_id: account_id_for_issuer_url(DEFAULT_ACME_DIRECTORY_URL),
            lifecycle: CertificateLifecycle::Pending { last_error: None },
            versions: Vec::new(),
            requested_at: 1,
            updated_at: 1,
        }
    }
}
