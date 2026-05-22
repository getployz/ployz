use std::time::SystemTime;

use crate::acme::{
    CertificateAuthorityPort, CertificateIssueOutcome, CertificateIssueRequest,
    CertificateIssuerPort,
};
use crate::error::CertificateFailure;
use crate::operation::MutationContext;

#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
pub(crate) struct AttemptingCertificateIssuer<A, I> {
    attempts: A,
    issuer: I,
}

#[allow(
    dead_code,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
impl<A, I> AttemptingCertificateIssuer<A, I> {
    #[must_use]
    pub(crate) fn new(attempts: A, issuer: I) -> Self {
        Self { attempts, issuer }
    }
}

impl<A, I> CertificateIssuerPort for AttemptingCertificateIssuer<A, I>
where
    A: polis::OperationBackend,
    I: CertificateAuthorityPort,
{
    fn issue_certificate(
        &self,
        context: &MutationContext,
        request: &CertificateIssueRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        let attempt_request = certificate_attempt_request(context, request)?;
        match polis::begin_typed_attempt(&self.attempts, attempt_request.clone())
            .map_err(map_typed_attempt_certificate_error)?
        {
            polis::TypedAttemptStart::Started(attempt) => {
                if let Err(failure) = self.issuer.issue_certificate(context, request) {
                    return match attempt.failed(&failure) {
                        Ok(()) => Err(failure),
                        Err(error) => self.replay_or_map_terminal_error(&attempt_request, error),
                    };
                }
                if let Err(error) = attempt.record(polis::OperationEvidence {
                    recorded_at: SystemTime::now(),
                    kind: polis::EvidenceKind::Checkpoint(b"certificate-issued".to_vec()),
                }) {
                    return self.replay_or_map_terminal_error(&attempt_request, error);
                }
                match attempt.succeeded() {
                    Ok(()) => Ok(CertificateIssueOutcome::Issued),
                    Err(error) => self.replay_or_map_terminal_error(&attempt_request, error),
                }
            }
            polis::TypedAttemptStart::Replayed(polis::TypedAttemptReplay::Succeeded { .. }) => {
                Ok(CertificateIssueOutcome::Replayed)
            }
            polis::TypedAttemptStart::Replayed(polis::TypedAttemptReplay::Open {
                operation,
                owner_deadline,
            }) if owner_deadline <= SystemTime::now() => {
                self.interrupt_expired_attempt(&attempt_request, &operation)
            }
            polis::TypedAttemptStart::Replayed(polis::TypedAttemptReplay::Open { .. }) => {
                Ok(CertificateIssueOutcome::InProgress)
            }
            polis::TypedAttemptStart::Replayed(polis::TypedAttemptReplay::Interrupted {
                ..
            }) => Ok(CertificateIssueOutcome::Interrupted),
            polis::TypedAttemptStart::Replayed(polis::TypedAttemptReplay::Failed {
                failure,
                ..
            }) => Err(failure),
        }
    }
}

#[allow(
    dead_code,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
impl<A, I> AttemptingCertificateIssuer<A, I>
where
    A: polis::OperationBackend,
{
    fn interrupt_expired_attempt(
        &self,
        request: &polis::TypedAttemptRequest<CertificateFailureCodec>,
        operation: &polis::OperationId,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match self
            .attempts
            .close(operation, polis::TerminalMarker::Interrupted)
        {
            Ok(()) => Ok(CertificateIssueOutcome::Interrupted),
            Err(polis::Error::TerminalAlreadyWritten) => {
                self.replay_after_terminal_conflict(request)
            }
            Err(error) => Err(map_polis_certificate_error(error)),
        }
    }

    fn replay_after_terminal_conflict(
        &self,
        request: &polis::TypedAttemptRequest<CertificateFailureCodec>,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match polis::replay_typed_attempt(&self.attempts, request)
            .map_err(map_typed_attempt_certificate_error)?
        {
            polis::TypedAttemptReplay::Succeeded { .. } => Ok(CertificateIssueOutcome::Replayed),
            polis::TypedAttemptReplay::Failed { failure, .. } => Err(failure),
            polis::TypedAttemptReplay::Interrupted { .. } => {
                Ok(CertificateIssueOutcome::Interrupted)
            }
            polis::TypedAttemptReplay::Open { .. } => Err(CertificateFailure::FreshnessUnknown),
        }
    }

    fn replay_or_map_terminal_error(
        &self,
        request: &polis::TypedAttemptRequest<CertificateFailureCodec>,
        error: polis::Error,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match error {
            polis::Error::TerminalAlreadyWritten => self.replay_after_terminal_conflict(request),
            other @ (polis::Error::Unauthorized
            | polis::Error::Conflict
            | polis::Error::Timeout
            | polis::Error::StaleFence
            | polis::Error::NoResponder
            | polis::Error::FreshnessUnknown
            | polis::Error::MalformedPayload) => Err(map_polis_certificate_error(other)),
        }
    }
}

#[allow(
    dead_code,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
fn certificate_attempt_request(
    context: &MutationContext,
    request: &CertificateIssueRequest,
) -> Result<polis::TypedAttemptRequest<CertificateFailureCodec>, CertificateFailure> {
    let hostname = request.binding.hostname.as_str();
    let actor = polis::PrincipalId::parse(context.authority().principal().as_str())
        .map_err(map_polis_certificate_error)?;
    let scope = polis::ScopeId::parse(context.authority().scope().as_str())
        .map_err(map_polis_certificate_error)?;
    let command =
        polis::CommandKind::parse("acme.issue_certificate").map_err(map_polis_certificate_error)?;
    let resource = polis::FingerprintedResource::parse(format!("cert:{hostname}"))
        .map_err(map_polis_certificate_error)?;
    let fingerprint = polis::RequestFingerprint::builder(
        actor,
        scope,
        command,
        "ployz.acme.certificate_issue.v1",
        polis::GrantEpoch::new(context.authority().epoch().value()),
    )
    .field("hostname", hostname)
    .field_time("minimum_valid_until", request.deadline.expires_at)
    .resource(resource)
    .finish()
    .map_err(map_polis_certificate_error)?;
    Ok(polis::TypedAttemptRequest::new(polis::AttemptRequest::new(
        polis::OperationId::parse(format!(
            "{}:acme-issue:{hostname}",
            context.operation().as_str()
        ))
        .map_err(map_polis_certificate_error)?,
        polis::IdempotencyKey::parse(format!(
            "{}:acme-issue:{hostname}",
            context.idempotency().as_str()
        ))
        .map_err(map_polis_certificate_error)?,
        fingerprint,
        context.deadline(),
    )))
}

#[allow(
    dead_code,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
fn map_polis_certificate_error(error: polis::Error) -> CertificateFailure {
    match error {
        polis::Error::Unauthorized | polis::Error::StaleFence => {
            CertificateFailure::UnauthorizedBinding
        }
        polis::Error::Timeout | polis::Error::FreshnessUnknown => {
            CertificateFailure::FreshnessUnknown
        }
        polis::Error::Conflict
        | polis::Error::NoResponder
        | polis::Error::MalformedPayload
        | polis::Error::TerminalAlreadyWritten => CertificateFailure::IssuanceFailed,
    }
}

#[allow(
    dead_code,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
fn map_typed_attempt_certificate_error(error: polis::TypedAttemptError) -> CertificateFailure {
    match error {
        polis::TypedAttemptError::Primitive(error) => map_polis_certificate_error(error),
        polis::TypedAttemptError::MalformedFailurePayload { .. } => {
            CertificateFailure::FreshnessUnknown
        }
    }
}

#[allow(
    dead_code,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
struct CertificateFailureCodec;

impl polis::AttemptFailureCodec for CertificateFailureCodec {
    type Failure = CertificateFailure;

    fn encode_failure(failure: &Self::Failure) -> Vec<u8> {
        match failure {
            CertificateFailure::UnauthorizedBinding => b"unauthorized_binding".to_vec(),
            CertificateFailure::ChallengeFailed => b"challenge_failed".to_vec(),
            CertificateFailure::IssuanceFailed => b"issuance_failed".to_vec(),
            CertificateFailure::MaterialUnsafe => b"material_unsafe".to_vec(),
            CertificateFailure::SafetyWindowFailed => b"safety_window_failed".to_vec(),
            CertificateFailure::KnownRevoked => b"known_revoked".to_vec(),
            CertificateFailure::FreshnessUnknown => b"freshness_unknown".to_vec(),
            CertificateFailure::ActivationRejected => b"activation_rejected".to_vec(),
        }
    }

    fn decode_failure(
        payload: &[u8],
    ) -> std::result::Result<Self::Failure, polis::AttemptFailureDecodeError> {
        match payload {
            b"unauthorized_binding" => Ok(CertificateFailure::UnauthorizedBinding),
            b"challenge_failed" => Ok(CertificateFailure::ChallengeFailed),
            b"issuance_failed" => Ok(CertificateFailure::IssuanceFailed),
            b"material_unsafe" => Ok(CertificateFailure::MaterialUnsafe),
            b"safety_window_failed" => Ok(CertificateFailure::SafetyWindowFailed),
            b"known_revoked" => Ok(CertificateFailure::KnownRevoked),
            b"freshness_unknown" => Ok(CertificateFailure::FreshnessUnknown),
            b"activation_rejected" => Ok(CertificateFailure::ActivationRejected),
            _ => Err(polis::AttemptFailureDecodeError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme::{
        CertificateDeadline, CertificateIssueRequest, CertificateIssuerPort, Hostname, HttpsBinding,
    };
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::rc::Rc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn attempt_issuer_records_only_non_secret_checkpoint_and_succeeds() {
        let backend = FakeOperationBackend::default();
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend.clone(), authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("issue");

        assert_eq!(outcome, CertificateIssueOutcome::Issued);
        assert_eq!(*authority.calls.borrow(), 1);
        assert_eq!(
            backend
                .terminal
                .borrow()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![polis::TerminalMarker::Succeeded]
        );
        let records = backend.records.borrow();
        assert_eq!(records.len(), 1);
        let Some(record) = records.first() else {
            panic!("expected recorded evidence");
        };
        let polis::EvidenceKind::Checkpoint(payload) = &record.kind else {
            panic!("expected checkpoint");
        };
        assert_eq!(payload, b"certificate-issued");
        assert!(
            !payload
                .windows(b"PRIVATE KEY".len())
                .any(|window| window == b"PRIVATE KEY")
        );
    }

    #[test]
    fn succeeded_replay_skips_external_issuer() {
        let backend = FakeOperationBackend::with_replay(Some(polis::TerminalMarker::Succeeded));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn open_replay_reports_in_progress_without_external_issuer() {
        let backend =
            FakeOperationBackend::with_open_replay(SystemTime::now() + Duration::from_secs(60));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::InProgress);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn expired_open_replay_terminalizes_interrupted_without_external_issuer() {
        let backend = FakeOperationBackend::with_open_replay(UNIX_EPOCH);
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend.clone(), authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Interrupted);
        assert_eq!(*authority.calls.borrow(), 0);
        assert_eq!(
            backend
                .terminal
                .borrow()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![polis::TerminalMarker::Interrupted]
        );
    }

    #[test]
    fn expired_open_replay_close_conflict_replays_success_terminal() {
        let backend = FakeOperationBackend::with_open_replay_then_close_conflict(
            UNIX_EPOCH,
            Some(polis::TerminalMarker::Succeeded),
        );
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn expired_open_replay_close_conflict_replays_failed_terminal() {
        let backend = FakeOperationBackend::with_open_replay_then_close_conflict(
            UNIX_EPOCH,
            Some(certificate_failed_marker(
                CertificateFailure::ChallengeFailed,
            )),
        );
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn external_issuer_failure_terminalizes_failed_attempt() {
        let backend = FakeOperationBackend::default();
        let authority = FakeCertificateAuthority::failing(CertificateFailure::ChallengeFailed);
        let issuer = AttemptingCertificateIssuer::new(backend.clone(), authority);

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failure");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(
            backend
                .terminal
                .borrow()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![certificate_failed_marker(
                CertificateFailure::ChallengeFailed
            )]
        );
    }

    #[test]
    fn failed_replay_preserves_certificate_failure_class() {
        let backend = FakeOperationBackend::with_replay(Some(certificate_failed_marker(
            CertificateFailure::ChallengeFailed,
        )));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn malformed_failed_replay_reports_freshness_unknown() {
        let backend = FakeOperationBackend::with_replay(Some(polis::TerminalMarker::Failed(
            b"unknown_certificate_failure".to_vec(),
        )));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("malformed replay");

        assert_eq!(error, CertificateFailure::FreshnessUnknown);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn started_success_close_conflict_replays_actual_success_terminal() {
        let backend = FakeOperationBackend::with_started_then_close_conflict(Some(
            polis::TerminalMarker::Succeeded,
        ));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[test]
    fn started_failure_close_conflict_replays_actual_success_terminal() {
        let backend = FakeOperationBackend::with_started_then_close_conflict(Some(
            polis::TerminalMarker::Succeeded,
        ));
        let authority = FakeCertificateAuthority::failing(CertificateFailure::ChallengeFailed);
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[test]
    fn started_success_close_conflict_replays_actual_failed_terminal() {
        let backend = FakeOperationBackend::with_started_then_close_conflict(Some(
            certificate_failed_marker(CertificateFailure::ChallengeFailed),
        ));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[derive(Clone, Default)]
    struct FakeOperationBackend {
        replays: Rc<RefCell<VecDeque<FakeOperationReplay>>>,
        records: Rc<RefCell<Vec<polis::OperationEvidence>>>,
        terminal: Rc<RefCell<BTreeMap<polis::OperationId, polis::TerminalMarker>>>,
        close_conflicts: Rc<RefCell<usize>>,
        starts_before_replay: Rc<RefCell<usize>>,
    }

    impl FakeOperationBackend {
        fn with_replay(terminal: Option<polis::TerminalMarker>) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline: UNIX_EPOCH + Duration::from_secs(60),
                    terminal,
                }]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(0)),
                starts_before_replay: Rc::new(RefCell::new(0)),
            }
        }

        fn with_open_replay(owner_deadline: SystemTime) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline,
                    terminal: None,
                }]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(0)),
                starts_before_replay: Rc::new(RefCell::new(0)),
            }
        }

        fn with_open_replay_then_close_conflict(
            owner_deadline: SystemTime,
            terminal: Option<polis::TerminalMarker>,
        ) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([
                    FakeOperationReplay {
                        owner_deadline,
                        terminal: None,
                    },
                    FakeOperationReplay {
                        owner_deadline,
                        terminal,
                    },
                ]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(1)),
                starts_before_replay: Rc::new(RefCell::new(0)),
            }
        }

        fn with_started_then_close_conflict(terminal: Option<polis::TerminalMarker>) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline: UNIX_EPOCH + Duration::from_secs(60),
                    terminal,
                }]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(1)),
                starts_before_replay: Rc::new(RefCell::new(1)),
            }
        }
    }

    #[derive(Clone)]
    struct FakeOperationReplay {
        owner_deadline: SystemTime,
        terminal: Option<polis::TerminalMarker>,
    }

    impl polis::OperationBackend for FakeOperationBackend {
        fn start_or_replay(
            &self,
            request: &polis::OperationRequest,
        ) -> polis::Result<polis::BackendOperationStart> {
            let mut starts_before_replay = self.starts_before_replay.borrow_mut();
            if *starts_before_replay > 0 {
                *starts_before_replay -= 1;
                return Ok(polis::BackendOperationStart::Started);
            }
            drop(starts_before_replay);
            if let Some(replay) = self.replays.borrow_mut().pop_front() {
                return Ok(polis::BackendOperationStart::Replayed {
                    operation: request.operation().clone(),
                    owner_deadline: replay.owner_deadline,
                    terminal: replay.terminal,
                });
            }
            Ok(polis::BackendOperationStart::Started)
        }

        fn record(
            &self,
            _operation: &polis::OperationId,
            evidence: polis::OperationEvidence,
        ) -> polis::Result<()> {
            self.records.borrow_mut().push(evidence);
            Ok(())
        }

        fn close(
            &self,
            operation: &polis::OperationId,
            marker: polis::TerminalMarker,
        ) -> polis::Result<()> {
            let mut close_conflicts = self.close_conflicts.borrow_mut();
            if *close_conflicts > 0 {
                *close_conflicts -= 1;
                return Err(polis::Error::TerminalAlreadyWritten);
            }
            self.terminal.borrow_mut().insert(operation.clone(), marker);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeCertificateAuthority {
        result: Result<(), CertificateFailure>,
        calls: Rc<RefCell<usize>>,
    }

    impl Default for FakeCertificateAuthority {
        fn default() -> Self {
            Self {
                result: Ok(()),
                calls: Rc::new(RefCell::new(0)),
            }
        }
    }

    impl FakeCertificateAuthority {
        fn failing(failure: CertificateFailure) -> Self {
            Self {
                result: Err(failure),
                calls: Rc::new(RefCell::new(0)),
            }
        }
    }

    impl CertificateAuthorityPort for FakeCertificateAuthority {
        fn issue_certificate(
            &self,
            _context: &MutationContext,
            _request: &CertificateIssueRequest,
        ) -> Result<(), CertificateFailure> {
            *self.calls.borrow_mut() += 1;
            self.result.clone()
        }
    }

    fn certificate_failed_marker(failure: CertificateFailure) -> polis::TerminalMarker {
        polis::TerminalMarker::Failed(
            <CertificateFailureCodec as polis::AttemptFailureCodec>::encode_failure(&failure),
        )
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
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn request() -> CertificateIssueRequest {
        CertificateIssueRequest::new(
            HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname")),
            CertificateDeadline {
                expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
            },
        )
    }
}
