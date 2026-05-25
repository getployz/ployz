use std::time::SystemTime;

use crate::acme::attempt;
use crate::acme::{
    CertificateAuthorityPort, CertificateIssueOutcome, CertificateIssueRequest,
    CertificateIssuerPort,
};
use crate::error::{CertificateFailure, PrimitiveFailure};
use crate::operation::MutationContext;

#[derive(Debug, Clone)]
pub(crate) struct AcmeCertificateIssuer<A, I> {
    attempts: A,
    issuer: I,
}

impl<A, I> AcmeCertificateIssuer<A, I> {
    #[must_use]
    pub(crate) fn new(attempts: A, issuer: I) -> Self {
        Self { attempts, issuer }
    }
}

impl<A, I> CertificateIssuerPort for AcmeCertificateIssuer<A, I>
where
    A: attempt::CertificateAttemptStore,
    I: CertificateAuthorityPort,
{
    async fn issue_certificate(
        &self,
        context: &MutationContext,
        request: &CertificateIssueRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        let attempt_request = attempt::CertificateAttemptRequest::for_issue(context, request);
        match self
            .attempts
            .begin(&attempt_request)
            .await
            .map_err(map_primitive_certificate_error)?
        {
            attempt::CertificateAttemptStart::Started => {
                self.run_started_attempt(context, request, &attempt_request)
                    .await
            }
            attempt::CertificateAttemptStart::Replayed(
                attempt::CertificateAttemptReplay::Succeeded { .. },
            ) => Ok(CertificateIssueOutcome::AlreadyIssued),
            attempt::CertificateAttemptStart::Replayed(
                attempt::CertificateAttemptReplay::Open { owner_deadline, .. },
            ) if owner_deadline <= SystemTime::now() => {
                self.interrupt_expired_attempt(&attempt_request).await
            }
            attempt::CertificateAttemptStart::Replayed(
                attempt::CertificateAttemptReplay::Open { .. },
            ) => Ok(CertificateIssueOutcome::InProgress),
            attempt::CertificateAttemptStart::Replayed(
                attempt::CertificateAttemptReplay::Interrupted { .. },
            ) => Ok(CertificateIssueOutcome::Interrupted),
            attempt::CertificateAttemptStart::Replayed(
                attempt::CertificateAttemptReplay::Failed { failure, .. },
            ) => Err(failure),
        }
    }
}

impl<A, I> AcmeCertificateIssuer<A, I>
where
    A: attempt::CertificateAttemptStore,
    I: CertificateAuthorityPort,
{
    async fn run_started_attempt(
        &self,
        context: &MutationContext,
        request: &CertificateIssueRequest,
        attempt_request: &attempt::CertificateAttemptRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match self.issuer.issue_certificate(context, request).await {
            Ok(()) => self.finish_succeeded_attempt(attempt_request).await,
            Err(failure) => self.finish_failed_attempt(attempt_request, failure).await,
        }
    }

    async fn finish_succeeded_attempt(
        &self,
        request: &attempt::CertificateAttemptRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match self
            .attempts
            .finish(request, attempt::CertificateAttemptTerminal::Succeeded)
            .await
        {
            Ok(()) => Ok(CertificateIssueOutcome::Issued),
            Err(error) => {
                self.replay_or_map_success_evidence_error(request, error)
                    .await
            }
        }
    }

    async fn finish_failed_attempt(
        &self,
        request: &attempt::CertificateAttemptRequest,
        failure: CertificateFailure,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match self
            .attempts
            .finish(
                request,
                attempt::CertificateAttemptTerminal::Failed(failure.clone()),
            )
            .await
        {
            Ok(()) => Err(failure),
            Err(PrimitiveFailure::OperationStateConflict) => {
                self.replay_after_terminal_conflict(request).await
            }
            Err(error) => Err(CertificateFailure::FailureRecordUnavailable {
                primary: Box::new(failure),
                evidence: Box::new(map_primitive_certificate_error(error)),
            }),
        }
    }
}

impl<A, I> AcmeCertificateIssuer<A, I>
where
    A: attempt::CertificateAttemptStore,
{
    async fn interrupt_expired_attempt(
        &self,
        request: &attempt::CertificateAttemptRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match self
            .attempts
            .finish(request, attempt::CertificateAttemptTerminal::Interrupted)
            .await
        {
            Ok(()) => Ok(CertificateIssueOutcome::Interrupted),
            Err(PrimitiveFailure::OperationStateConflict) => {
                self.replay_after_terminal_conflict(request).await
            }
            Err(error) => Err(map_primitive_certificate_error(error)),
        }
    }

    async fn replay_after_terminal_conflict(
        &self,
        request: &attempt::CertificateAttemptRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match self
            .attempts
            .begin(request)
            .await
            .map_err(map_primitive_certificate_error)?
        {
            attempt::CertificateAttemptStart::Started => Err(CertificateFailure::IssuanceFailed),
            attempt::CertificateAttemptStart::Replayed(replay) => Self::replay_outcome(replay),
        }
    }

    fn replay_outcome(
        replay: attempt::CertificateAttemptReplay,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match replay {
            attempt::CertificateAttemptReplay::Succeeded { .. } => {
                Ok(CertificateIssueOutcome::AlreadyIssued)
            }
            attempt::CertificateAttemptReplay::Failed { failure, .. } => Err(failure),
            attempt::CertificateAttemptReplay::Interrupted { .. } => {
                Ok(CertificateIssueOutcome::Interrupted)
            }
            attempt::CertificateAttemptReplay::Open { .. } => {
                Err(CertificateFailure::FreshnessUnknown)
            }
        }
    }

    async fn replay_or_map_success_evidence_error(
        &self,
        request: &attempt::CertificateAttemptRequest,
        error: PrimitiveFailure,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match error {
            PrimitiveFailure::OperationStateConflict => {
                self.replay_after_terminal_conflict(request).await
            }
            PrimitiveFailure::Unauthorized
            | PrimitiveFailure::Conflict
            | PrimitiveFailure::Timeout
            | PrimitiveFailure::StaleFence
            | PrimitiveFailure::NoResponder
            | PrimitiveFailure::FreshnessUnknown
            | PrimitiveFailure::MalformedPayload
            | PrimitiveFailure::OperationAlreadySucceeded
            | PrimitiveFailure::OperationInProgress
            | PrimitiveFailure::OperationAlreadyFailed
            | PrimitiveFailure::OperationInterrupted => Err(CertificateFailure::FreshnessUnknown),
        }
    }
}

fn map_primitive_certificate_error(error: PrimitiveFailure) -> CertificateFailure {
    match error {
        PrimitiveFailure::Unauthorized | PrimitiveFailure::StaleFence => {
            CertificateFailure::UnauthorizedBinding
        }
        PrimitiveFailure::Timeout | PrimitiveFailure::FreshnessUnknown => {
            CertificateFailure::FreshnessUnknown
        }
        PrimitiveFailure::Conflict
        | PrimitiveFailure::NoResponder
        | PrimitiveFailure::MalformedPayload
        | PrimitiveFailure::OperationStateConflict
        | PrimitiveFailure::OperationAlreadySucceeded
        | PrimitiveFailure::OperationInProgress
        | PrimitiveFailure::OperationAlreadyFailed
        | PrimitiveFailure::OperationInterrupted => CertificateFailure::IssuanceFailed,
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

    #[tokio::test(flavor = "current_thread")]
    async fn attempt_issuer_terminalizes_success() {
        let backend = FakeAttemptBackend::default();
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend.clone(), authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .await
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
            vec![attempt::CertificateAttemptTerminal::Succeeded]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn succeeded_replay_skips_external_issuer() {
        let backend =
            FakeAttemptBackend::with_replay(Some(attempt::CertificateAttemptTerminal::Succeeded));
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::AlreadyIssued);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn open_replay_reports_in_progress_without_external_issuer() {
        let backend =
            FakeAttemptBackend::with_open_replay(SystemTime::now() + Duration::from_secs(60));
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::InProgress);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expired_open_replay_terminalizes_interrupted_without_external_issuer() {
        let backend = FakeAttemptBackend::with_open_replay(UNIX_EPOCH);
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend.clone(), authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .await
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
            vec![attempt::CertificateAttemptTerminal::Interrupted]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expired_open_replay_close_conflict_replays_success_terminal() {
        let backend = FakeAttemptBackend::with_open_replay_then_close_conflict(
            UNIX_EPOCH,
            Some(attempt::CertificateAttemptTerminal::Succeeded),
        );
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::AlreadyIssued);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expired_open_replay_close_conflict_replays_failed_terminal() {
        let backend = FakeAttemptBackend::with_open_replay_then_close_conflict(
            UNIX_EPOCH,
            Some(certificate_failed_marker(
                CertificateFailure::ChallengeFailed,
            )),
        );
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn external_issuer_failure_terminalizes_failed_attempt() {
        let backend = FakeAttemptBackend::default();
        let authority = FakeCertificateAuthority::failing(CertificateFailure::ChallengeFailed);
        let issuer = AcmeCertificateIssuer::new(backend.clone(), authority);

        let error = issuer
            .issue_certificate(&context(), &request())
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn failed_replay_preserves_certificate_failure_class() {
        let backend = FakeAttemptBackend::with_replay(Some(certificate_failed_marker(
            CertificateFailure::ChallengeFailed,
        )));
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn started_success_close_conflict_replays_actual_success_terminal() {
        let backend = FakeAttemptBackend::with_started_then_close_conflict(Some(
            attempt::CertificateAttemptTerminal::Succeeded,
        ));
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::AlreadyIssued);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn started_failure_close_conflict_replays_actual_success_terminal() {
        let backend = FakeAttemptBackend::with_started_then_close_conflict(Some(
            attempt::CertificateAttemptTerminal::Succeeded,
        ));
        let authority = FakeCertificateAuthority::failing(CertificateFailure::ChallengeFailed);
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::AlreadyIssued);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn started_success_close_conflict_replays_actual_failed_terminal() {
        let backend = FakeAttemptBackend::with_started_then_close_conflict(Some(
            certificate_failed_marker(CertificateFailure::ChallengeFailed),
        ));
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_external_issue_with_terminal_write_failure_reports_unknown_evidence() {
        let backend = FakeAttemptBackend::with_finish_error(PrimitiveFailure::NoResponder);
        let authority = FakeCertificateAuthority::default();
        let issuer = AcmeCertificateIssuer::new(backend.clone(), authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect_err("terminal evidence write should fail");

        assert_eq!(error, CertificateFailure::FreshnessUnknown);
        assert_eq!(*authority.calls.borrow(), 1);
        assert!(backend.terminal.borrow().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_external_issue_with_terminal_write_failure_preserves_primary_failure() {
        let backend = FakeAttemptBackend::with_finish_error(PrimitiveFailure::NoResponder);
        let authority = FakeCertificateAuthority::failing(CertificateFailure::ChallengeFailed);
        let issuer = AcmeCertificateIssuer::new(backend.clone(), authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .await
            .expect_err("terminal evidence write should attach to primary failure");

        assert_eq!(
            error,
            CertificateFailure::FailureRecordUnavailable {
                primary: Box::new(CertificateFailure::ChallengeFailed),
                evidence: Box::new(CertificateFailure::IssuanceFailed),
            }
        );
        assert_eq!(*authority.calls.borrow(), 1);
        assert!(backend.terminal.borrow().is_empty());
    }

    #[derive(Clone, Default)]
    struct FakeAttemptBackend {
        replays: Rc<RefCell<VecDeque<FakeOperationReplay>>>,
        terminal: Rc<RefCell<BTreeMap<IdempotencyKey, attempt::CertificateAttemptTerminal>>>,
        close_conflicts: Rc<RefCell<usize>>,
        starts_before_replay: Rc<RefCell<usize>>,
        finish_errors: Rc<RefCell<VecDeque<PrimitiveFailure>>>,
    }

    impl FakeAttemptBackend {
        fn with_replay(terminal: Option<attempt::CertificateAttemptTerminal>) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline: UNIX_EPOCH + Duration::from_secs(60),
                    terminal,
                }]))),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(0)),
                starts_before_replay: Rc::new(RefCell::new(0)),
                finish_errors: Rc::default(),
            }
        }

        fn with_open_replay(owner_deadline: SystemTime) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline,
                    terminal: None,
                }]))),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(0)),
                starts_before_replay: Rc::new(RefCell::new(0)),
                finish_errors: Rc::default(),
            }
        }

        fn with_open_replay_then_close_conflict(
            owner_deadline: SystemTime,
            terminal: Option<attempt::CertificateAttemptTerminal>,
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
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(1)),
                starts_before_replay: Rc::new(RefCell::new(0)),
                finish_errors: Rc::default(),
            }
        }

        fn with_started_then_close_conflict(
            terminal: Option<attempt::CertificateAttemptTerminal>,
        ) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline: UNIX_EPOCH + Duration::from_secs(60),
                    terminal,
                }]))),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(1)),
                starts_before_replay: Rc::new(RefCell::new(1)),
                finish_errors: Rc::default(),
            }
        }

        fn with_finish_error(error: PrimitiveFailure) -> Self {
            let backend = Self::default();
            backend.finish_errors.borrow_mut().push_back(error);
            backend
        }
    }

    #[derive(Clone)]
    struct FakeOperationReplay {
        owner_deadline: SystemTime,
        terminal: Option<attempt::CertificateAttemptTerminal>,
    }

    impl attempt::CertificateAttemptStore for FakeAttemptBackend {
        async fn begin(
            &self,
            request: &attempt::CertificateAttemptRequest,
        ) -> Result<attempt::CertificateAttemptStart, PrimitiveFailure> {
            let mut starts_before_replay = self.starts_before_replay.borrow_mut();
            if *starts_before_replay > 0 {
                *starts_before_replay -= 1;
                return Ok(attempt::CertificateAttemptStart::Started);
            }
            drop(starts_before_replay);
            if let Some(replay) = self.replays.borrow_mut().pop_front() {
                return Ok(attempt::CertificateAttemptStart::Replayed(replay_from(
                    request.operation().clone(),
                    replay,
                )));
            }
            Ok(attempt::CertificateAttemptStart::Started)
        }

        async fn finish(
            &self,
            request: &attempt::CertificateAttemptRequest,
            marker: attempt::CertificateAttemptTerminal,
        ) -> Result<(), PrimitiveFailure> {
            if let Some(error) = self.finish_errors.borrow_mut().pop_front() {
                return Err(error);
            }
            let mut close_conflicts = self.close_conflicts.borrow_mut();
            if *close_conflicts > 0 {
                *close_conflicts -= 1;
                return Err(PrimitiveFailure::OperationStateConflict);
            }
            self.terminal
                .borrow_mut()
                .insert(request.idempotency().clone(), marker);
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
        async fn issue_certificate(
            &self,
            _context: &MutationContext,
            _request: &CertificateIssueRequest,
        ) -> Result<(), CertificateFailure> {
            *self.calls.borrow_mut() += 1;
            self.result.clone()
        }
    }

    fn replay_from(
        operation: OperationId,
        replay: FakeOperationReplay,
    ) -> attempt::CertificateAttemptReplay {
        match replay.terminal {
            Some(attempt::CertificateAttemptTerminal::Succeeded) => {
                attempt::CertificateAttemptReplay::Succeeded { operation }
            }
            Some(attempt::CertificateAttemptTerminal::Failed(failure)) => {
                attempt::CertificateAttemptReplay::Failed { operation, failure }
            }
            Some(attempt::CertificateAttemptTerminal::Interrupted) => {
                attempt::CertificateAttemptReplay::Interrupted { operation }
            }
            None => attempt::CertificateAttemptReplay::Open {
                operation,
                owner_deadline: replay.owner_deadline,
            },
        }
    }

    fn certificate_failed_marker(
        failure: CertificateFailure,
    ) -> attempt::CertificateAttemptTerminal {
        attempt::CertificateAttemptTerminal::Failed(failure)
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
