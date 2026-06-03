//! Composition layer that wires Ployz product ports to adapters.
//!
//! Feature modules should depend on Ployz-owned traits. This module is allowed
//! to assemble concrete adapters and pass them into product orchestration.

use crate::acme::{
    AcmeCertificateIssuer, CertificateAuthorityPort, CertificatePort, CertificateReadinessService,
    CertificateStatusPort, attempt,
};
#[cfg(any(test, feature = "test-support"))]
use crate::adapters::memory::{
    InMemoryDomainStatus, InMemoryMachineMembership, InMemoryServingSnapshots,
};
use crate::adapters::polis::{
    certificate_attempt_schema_statements, domain_status_schema_statements,
    serving_snapshot_schema_statements, start_corrosion_certificate_attempts,
    start_corrosion_domain_status, start_corrosion_machine_membership,
    start_corrosion_serving_snapshots, verify_certificate_attempt_schema,
    verify_domain_status_schema, verify_serving_snapshot_schema,
};
use crate::domain::DomainStatusPort;
use crate::error::PrimitiveFailure;
use crate::machine::MachineMembershipPort;
use crate::serving::ServingSnapshotPort;

#[must_use]
pub fn certificate_readiness_with_attempts<S, A, I>(
    certificates: S,
    attempts: A,
    issuer: I,
) -> impl CertificatePort
where
    S: CertificateStatusPort,
    A: attempt::CertificateAttemptStore,
    I: CertificateAuthorityPort,
{
    CertificateReadinessService::new(certificates, AcmeCertificateIssuer::new(attempts, issuer))
}

#[must_use]
#[cfg(any(test, feature = "test-support"))]
pub fn in_memory_machine_membership() -> impl MachineMembershipPort {
    InMemoryMachineMembership::default()
}

#[must_use]
pub fn corrosion_machine_membership<P>(
    store: polis::CorrosionStore,
    probe: P,
    island: polis::IslandId,
) -> impl MachineMembershipPort
where
    P: polis::PeerProbe,
{
    start_corrosion_machine_membership(store, probe, island)
}

#[must_use]
pub fn corrosion_domain_status(store: polis::CorrosionStore) -> impl DomainStatusPort {
    start_corrosion_domain_status(store)
}

#[must_use]
pub fn corrosion_certificate_attempts(
    store: polis::CorrosionStore,
) -> impl attempt::CertificateAttemptStore {
    start_corrosion_certificate_attempts(store)
}

#[must_use]
pub fn corrosion_serving_snapshots(store: polis::CorrosionStore) -> impl ServingSnapshotPort {
    start_corrosion_serving_snapshots(store)
}

pub fn product_schema_statements() -> Result<Vec<polis::StoreStatement>, PrimitiveFailure> {
    let mut statements = Vec::new();
    for schema in PRODUCT_SCHEMAS {
        statements.extend(schema.statements().map_err(map_product_schema_error)?);
    }
    Ok(statements)
}

pub async fn verify_product_schema(
    store: &polis::CorrosionStore,
    timeout: polis::StoreTimeout,
) -> Result<(), PrimitiveFailure> {
    for schema in PRODUCT_SCHEMAS {
        schema
            .verify(store, timeout)
            .await
            .map_err(map_product_schema_error)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ProductSchema {
    DomainStatus,
    CertificateAttempt,
    ServingSnapshot,
}

const PRODUCT_SCHEMAS: &[ProductSchema] = &[
    ProductSchema::DomainStatus,
    ProductSchema::CertificateAttempt,
    ProductSchema::ServingSnapshot,
];

impl ProductSchema {
    fn statements(self) -> Result<Vec<polis::StoreStatement>, polis::StoreError> {
        match self {
            Self::DomainStatus => domain_status_schema_statements(),
            Self::CertificateAttempt => certificate_attempt_schema_statements(),
            Self::ServingSnapshot => serving_snapshot_schema_statements(),
        }
    }

    async fn verify(
        self,
        store: &polis::CorrosionStore,
        timeout: polis::StoreTimeout,
    ) -> Result<(), polis::StoreError> {
        match self {
            Self::DomainStatus => verify_domain_status_schema(store, timeout).await,
            Self::CertificateAttempt => verify_certificate_attempt_schema(store, timeout).await,
            Self::ServingSnapshot => verify_serving_snapshot_schema(store, timeout).await,
        }
    }
}

pub fn iroh_peer_rpc_probe(
    endpoint: polis::PeerEndpoint,
    ticket: &polis::PeerTicket,
) -> Result<impl polis::PeerProbe, PrimitiveFailure> {
    polis::PeerRpcProbe::connect(endpoint, ticket).map_err(map_peer_probe_error)
}

fn map_peer_probe_error(error: polis::PeerError) -> PrimitiveFailure {
    match error {
        polis::PeerError::MalformedTicket | polis::PeerError::MalformedIdentity => {
            PrimitiveFailure::MalformedPayload
        }
        polis::PeerError::IdentityIo { .. }
        | polis::PeerError::EndpointBind { .. }
        | polis::PeerError::EndpointOnlineTimeout => PrimitiveFailure::OperationInterrupted,
        polis::PeerError::RpcTimeout => PrimitiveFailure::Timeout,
        polis::PeerError::ProbeFailed { .. }
        | polis::PeerError::RpcTransport { .. }
        | polis::PeerError::RpcRuntime { .. } => PrimitiveFailure::NoResponder,
    }
}

fn map_product_schema_error(error: polis::StoreError) -> PrimitiveFailure {
    match error {
        polis::StoreError::MalformedPayload => PrimitiveFailure::MalformedPayload,
        polis::StoreError::Timeout => PrimitiveFailure::Timeout,
        polis::StoreError::MissedChange { .. }
        | polis::StoreError::Stream { .. }
        | polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryChangedBeforeEndOfQuery
        | polis::StoreError::QueryEndedBeforeEndOfQuery => PrimitiveFailure::NoResponder,
    }
}

#[must_use]
#[cfg(any(test, feature = "test-support"))]
pub fn in_memory_domain_status() -> impl DomainStatusPort {
    InMemoryDomainStatus::default()
}

#[must_use]
#[cfg(any(test, feature = "test-support"))]
pub fn in_memory_serving_snapshots() -> impl ServingSnapshotPort {
    InMemoryServingSnapshots::default()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::acme::{
        CertificateActivation, CertificateDeadline, CertificateIssueRequest,
        CertificateMaterialState, CertificateStatus, CertificateUsability,
        EnsureCertificateOutcome, Hostname, HttpsBinding, RevocationFreshness,
    };
    use crate::error::CertificateFailure;
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, MutationContext, OperationId,
        PrincipalId, ScopeId,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn certificate_readiness_composition_uses_attempt_adapter_for_missing_cert() {
        let status = SequenceCertificateStatus::new(vec![
            CertificateStatus::Absent,
            CertificateStatus::Present(usable_certificate()),
        ]);
        let attempts = RecordingAttemptBackend::default();
        let issuer = RecordingCertificateAuthority::default();
        let service = certificate_readiness_with_attempts(status, attempts.clone(), issuer.clone());

        let outcome = service
            .ensure_usable(&context(), &binding(), deadline())
            .await
            .expect("ensure certificate");

        assert!(matches!(outcome, EnsureCertificateOutcome::Usable(_)));
        assert_eq!(*issuer.calls.borrow(), 1);
        assert_eq!(
            attempts.terminals.borrow().as_slice(),
            &[attempt::CertificateAttemptTerminal::Succeeded]
        );
    }

    #[test]
    fn peer_runtime_starts_identity_bound_rpc_listener() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let identity_path = temp_identity_path();
            let peer = polis::PeerRuntime::start(
                &identity_path,
                polis::PeerProbeDeadline::new(Duration::from_secs(5)),
            )
            .await
            .expect("peer runtime");
            let persisted = polis::load_or_create_identity(&identity_path).expect("identity");

            assert_eq!(peer.endpoint_id(), persisted.endpoint_id());

            let client_identity = polis::PeerIdentity::generate();
            let client_endpoint = polis::bind_peer_endpoint(&client_identity)
                .await
                .expect("client endpoint");
            let client = polis::PeerRpcClient::connect(
                client_endpoint.clone(),
                peer.ticket().endpoint_addr().clone(),
            );
            client
                .preflight(polis::PeerProbeDeadline::new(Duration::from_secs(5)))
                .await
                .expect("preflight");

            client_endpoint.close().await;
            peer.shutdown(polis::PeerProbeDeadline::new(Duration::from_secs(5)))
                .await
                .expect("shutdown");
            let _ = fs::remove_file(identity_path);
        });
    }

    #[derive(Clone)]
    struct SequenceCertificateStatus {
        statuses: Rc<RefCell<VecDeque<CertificateStatus>>>,
    }

    impl SequenceCertificateStatus {
        fn new(statuses: Vec<CertificateStatus>) -> Self {
            Self {
                statuses: Rc::new(RefCell::new(statuses.into())),
            }
        }
    }

    impl CertificateStatusPort for SequenceCertificateStatus {
        async fn status(
            &self,
            _binding: &HttpsBinding,
        ) -> Result<CertificateStatus, CertificateFailure> {
            Ok(self
                .statuses
                .borrow_mut()
                .pop_front()
                .unwrap_or(CertificateStatus::Unknown))
        }
    }

    #[derive(Clone, Default)]
    struct RecordingCertificateAuthority {
        calls: Rc<RefCell<usize>>,
    }

    impl CertificateAuthorityPort for RecordingCertificateAuthority {
        async fn issue_certificate(
            &self,
            _context: &MutationContext,
            _request: &CertificateIssueRequest,
        ) -> Result<(), CertificateFailure> {
            *self.calls.borrow_mut() += 1;
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingAttemptBackend {
        terminals: Rc<RefCell<Vec<attempt::CertificateAttemptTerminal>>>,
    }

    impl attempt::CertificateAttemptStore for RecordingAttemptBackend {
        async fn begin(
            &self,
            _request: &attempt::CertificateAttemptRequest,
        ) -> Result<attempt::CertificateAttemptStart, PrimitiveFailure> {
            Ok(attempt::CertificateAttemptStart::Started)
        }

        async fn finish(
            &self,
            _request: &attempt::CertificateAttemptRequest,
            marker: attempt::CertificateAttemptTerminal,
        ) -> Result<(), PrimitiveFailure> {
            self.terminals.borrow_mut().push(marker);
            Ok(())
        }
    }

    fn binding() -> HttpsBinding {
        HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname"))
    }

    fn deadline() -> CertificateDeadline {
        CertificateDeadline {
            expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
        }
    }

    fn usable_certificate() -> CertificateUsability {
        CertificateUsability {
            hostname: binding().hostname,
            not_after: UNIX_EPOCH + Duration::from_secs(7_200),
            activation: CertificateActivation::Acknowledged,
            material: CertificateMaterialState::PresentProtected,
            revocation: RevocationFreshness::KnownFresh,
        }
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("deploy-1").expect("operation"),
            IdempotencyKey::parse("deploy-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            SystemTime::now() + Duration::from_secs(60),
        )
    }

    fn temp_identity_path() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ployz-peer-runtime-{}-{id}.key",
            std::process::id()
        ))
    }
}
