//! Composition layer that wires Ployz product ports to adapters.
//!
//! Feature modules should depend on Ployz-owned traits. This module is allowed
//! to assemble concrete adapters and pass them into product orchestration.

use crate::acme::{
    CertificateAuthorityPort, CertificatePort, CertificateReadinessService, CertificateStatusPort,
};
use crate::adapters::polis::{
    AcmeCertificateIssuer, PolisDomainStatus, PolisMachineMembership, PolisServingSnapshots,
};
use crate::domain::DomainStatusPort;
use crate::machine::MachineMembershipPort;
use crate::operation::ScopeId;
use crate::serving::ServingSnapshotPort;
use polis::external_attempt as attempt;

#[must_use]
pub fn certificate_readiness_with_attempts<S, A, I>(
    certificates: S,
    attempts: A,
    issuer: I,
) -> impl CertificatePort
where
    S: CertificateStatusPort,
    A: attempt::Backend,
    I: CertificateAuthorityPort,
{
    CertificateReadinessService::new(certificates, AcmeCertificateIssuer::new(attempts, issuer))
}

#[must_use]
pub fn in_memory_machine_membership() -> impl MachineMembershipPort {
    PolisMachineMembership::in_memory()
}

#[must_use]
pub fn in_memory_domain_status(scope: ScopeId) -> impl DomainStatusPort {
    PolisDomainStatus::in_memory(scope)
}

#[must_use]
pub fn in_memory_serving_snapshots(scope: ScopeId) -> impl ServingSnapshotPort {
    PolisServingSnapshots::in_memory(scope)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    #[test]
    fn certificate_readiness_composition_uses_attempt_adapter_for_missing_cert() {
        let status = SequenceCertificateStatus::new(vec![
            CertificateStatus::Absent,
            CertificateStatus::Present(usable_certificate()),
        ]);
        let attempts = RecordingAttemptBackend::default();
        let issuer = RecordingCertificateAuthority::default();
        let service = certificate_readiness_with_attempts(status, attempts.clone(), issuer.clone());

        let outcome = service
            .ensure_usable(&context(), &binding(), deadline())
            .expect("ensure certificate");

        assert!(matches!(outcome, EnsureCertificateOutcome::Usable(_)));
        assert_eq!(*issuer.calls.borrow(), 1);
        assert_eq!(
            attempts.terminals.borrow().as_slice(),
            &[attempt::TerminalMarker::Succeeded]
        );
        let records = attempts.records.borrow();
        let Some(record) = records.first() else {
            panic!("expected certificate issuance checkpoint");
        };
        let attempt::EvidenceKind::Checkpoint(payload) = &record.kind else {
            panic!("expected checkpoint evidence");
        };
        assert_eq!(payload, b"certificate-issued");
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
        fn status(&self, _binding: &HttpsBinding) -> Result<CertificateStatus, CertificateFailure> {
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
        fn issue_certificate(
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
        records: Rc<RefCell<Vec<attempt::Evidence>>>,
        terminals: Rc<RefCell<Vec<attempt::TerminalMarker>>>,
    }

    impl attempt::Backend for RecordingAttemptBackend {
        fn start_or_replay(
            &self,
            _request: &attempt::BackendRequest,
        ) -> polis::Result<attempt::BackendStart> {
            Ok(attempt::BackendStart::Started)
        }

        fn record(
            &self,
            _operation: &attempt::OperationId,
            evidence: attempt::Evidence,
        ) -> polis::Result<()> {
            self.records.borrow_mut().push(evidence);
            Ok(())
        }

        fn close(
            &self,
            _operation: &attempt::OperationId,
            marker: attempt::TerminalMarker,
        ) -> polis::Result<()> {
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
}
