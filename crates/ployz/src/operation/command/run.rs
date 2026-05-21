use std::time::SystemTime;

use polis::{AttemptReplay, EvidenceKind, OperationBackend, OperationEvidence};

use crate::error::PrimitiveFailure;
use crate::operation::command::issue::IssuedAttempt;
use crate::operation::context::MutationContext;
use crate::operation::polis_boundary::map_polis_to_primitive;

pub struct AttemptContext<'a> {
    mutation: MutationContext,
    attempt: polis::OpenAttempt<'a>,
}

impl<'a> AttemptContext<'a> {
    #[must_use]
    pub(crate) fn new(mutation: MutationContext, attempt: polis::OpenAttempt<'a>) -> Self {
        Self { mutation, attempt }
    }

    #[must_use]
    pub fn mutation(&self) -> &MutationContext {
        &self.mutation
    }

    pub(crate) fn checkpoint(&self, checkpoint: AttemptCheckpoint) -> Result<(), PrimitiveFailure> {
        self.attempt
            .record(OperationEvidence {
                recorded_at: SystemTime::now(),
                kind: EvidenceKind::Checkpoint(checkpoint.encode()),
            })
            .map_err(map_polis_to_primitive)
    }

    #[must_use]
    fn into_attempt(self) -> polis::OpenAttempt<'a> {
        self.attempt
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttemptCheckpoint {
    name: &'static str,
    fields: Vec<CheckpointField>,
}

impl AttemptCheckpoint {
    #[must_use]
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            fields: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn field(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.fields.push(CheckpointField {
            key,
            value: value.into(),
        });
        self
    }

    #[must_use]
    fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(self.name.as_bytes());
        payload.push(b';');
        for field in &self.fields {
            field.encode_into(&mut payload);
        }
        payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckpointField {
    key: &'static str,
    value: String,
}

impl CheckpointField {
    fn encode_into(&self, payload: &mut Vec<u8>) {
        payload.extend_from_slice(self.key.as_bytes());
        payload.push(b'=');
        payload.extend_from_slice(self.value.len().to_string().as_bytes());
        payload.push(b':');
        payload.extend_from_slice(self.value.as_bytes());
        payload.push(b';');
    }
}

mod sealed {
    pub trait Sealed {}
}

pub trait AttemptBackend: sealed::Sealed {
    fn run<C, T, E, F>(&self, envelope: IssuedAttempt<C>, work: F) -> Result<T, E>
    where
        E: AttemptProductError,
        F: FnOnce(&AttemptContext<'_>) -> Result<T, E>;

    fn run_with_replay<C, T, E, F, R>(
        &self,
        envelope: IssuedAttempt<C>,
        work: F,
        verify_replayed_success: R,
    ) -> Result<T, E>
    where
        E: AttemptProductError,
        F: FnOnce(&AttemptContext<'_>) -> Result<T, E>,
        R: FnOnce(&MutationContext) -> Result<T, E>;

    fn run_with_replay_and_failure_disposition<C, T, E, F, R>(
        &self,
        envelope: IssuedAttempt<C>,
        failure_disposition: AttemptFailureDisposition,
        work: F,
        verify_replayed_success: R,
    ) -> Result<T, E>
    where
        E: AttemptProductError,
        F: FnOnce(&AttemptContext<'_>) -> Result<T, E>,
        R: FnOnce(&MutationContext) -> Result<T, E>;
}

pub trait AttemptProductError: Sized {
    fn from_primitive_failure(error: PrimitiveFailure) -> Self;

    fn terminalization_failed(product: Self, terminalization: PrimitiveFailure) -> Self;
}

impl AttemptProductError for PrimitiveFailure {
    fn from_primitive_failure(error: PrimitiveFailure) -> Self {
        error
    }

    fn terminalization_failed(_product: Self, terminalization: PrimitiveFailure) -> Self {
        terminalization
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptFailureDisposition {
    Failed,
    Interrupted,
}

impl AttemptFailureDisposition {
    fn terminalize(self, attempt: polis::OpenAttempt<'_>) -> polis::Result<()> {
        match self {
            Self::Failed => attempt.failed(Vec::new()),
            Self::Interrupted => attempt.interrupted(),
        }
    }
}

pub struct AttemptLog<O> {
    operations: O,
}

impl<O> AttemptLog<O> {
    #[must_use]
    pub fn new(operations: O) -> Self {
        Self { operations }
    }
}

impl<O> AttemptBackend for AttemptLog<O>
where
    O: OperationBackend,
{
    fn run<C, T, E, F>(&self, envelope: IssuedAttempt<C>, work: F) -> Result<T, E>
    where
        E: AttemptProductError,
        F: FnOnce(&AttemptContext<'_>) -> Result<T, E>,
    {
        self.run_with_replay(envelope, work, |_replay| {
            Err(E::from_primitive_failure(
                PrimitiveFailure::OperationAlreadySucceeded,
            ))
        })
    }

    fn run_with_replay<C, T, E, F, R>(
        &self,
        envelope: IssuedAttempt<C>,
        work: F,
        verify_replayed_success: R,
    ) -> Result<T, E>
    where
        E: AttemptProductError,
        F: FnOnce(&AttemptContext<'_>) -> Result<T, E>,
        R: FnOnce(&MutationContext) -> Result<T, E>,
    {
        self.run_with_replay_and_failure_disposition(
            envelope,
            AttemptFailureDisposition::Failed,
            work,
            verify_replayed_success,
        )
    }

    fn run_with_replay_and_failure_disposition<C, T, E, F, R>(
        &self,
        envelope: IssuedAttempt<C>,
        failure_disposition: AttemptFailureDisposition,
        work: F,
        verify_replayed_success: R,
    ) -> Result<T, E>
    where
        E: AttemptProductError,
        F: FnOnce(&AttemptContext<'_>) -> Result<T, E>,
        R: FnOnce(&MutationContext) -> Result<T, E>,
    {
        let mutation = envelope.context().clone();
        let start = polis::begin_attempt(&self.operations, envelope.operation_request)
            .map_err(map_polis_to_primitive)
            .map_err(E::from_primitive_failure)?;

        let attempt = match start {
            polis::AttemptStart::Started(attempt) => attempt,
            polis::AttemptStart::Replayed(replay) => match replay {
                AttemptReplay::Succeeded { .. } => return verify_replayed_success(&mutation),
                AttemptReplay::Open { .. } => {
                    return Err(E::from_primitive_failure(
                        PrimitiveFailure::OperationInProgress,
                    ));
                }
                AttemptReplay::Failed { .. } => {
                    return Err(E::from_primitive_failure(
                        PrimitiveFailure::OperationAlreadyFailed,
                    ));
                }
                AttemptReplay::Interrupted { .. } => {
                    return Err(E::from_primitive_failure(
                        PrimitiveFailure::OperationInterrupted,
                    ));
                }
            },
        };

        let context = AttemptContext::new(mutation, attempt);

        match work(&context) {
            Ok(value) => {
                context
                    .into_attempt()
                    .succeeded()
                    .map_err(map_polis_to_primitive)
                    .map_err(E::from_primitive_failure)?;
                Ok(value)
            }
            Err(error) => match failure_disposition.terminalize(context.into_attempt()) {
                Ok(()) => Err(error),
                Err(terminalization) => Err(E::terminalization_failed(
                    error,
                    map_polis_to_primitive(terminalization),
                )),
            },
        }
    }
}

impl<O> sealed::Sealed for AttemptLog<O> where O: OperationBackend {}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use polis::{
        BackendOperationStart, CommandKind, FingerprintedResource,
        OperationId as BackendOperationId, OperationRequest, TerminalMarker,
    };

    use super::*;
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };

    #[derive(Clone, Default)]
    struct FakeOperations {
        evidence: Rc<RefCell<Vec<EvidenceKind>>>,
        terminal: Rc<RefCell<Vec<TerminalMarker>>>,
        operation: Rc<RefCell<Vec<BackendOperationId>>>,
        replay: Option<Option<TerminalMarker>>,
        close_error: Option<polis::Error>,
    }

    impl OperationBackend for FakeOperations {
        fn start_or_replay(
            &self,
            request: &OperationRequest,
        ) -> polis::Result<BackendOperationStart> {
            if let Some(terminal) = self.replay.clone() {
                return Ok(BackendOperationStart::Replayed {
                    operation: request.operation().clone(),
                    terminal,
                });
            }
            Ok(BackendOperationStart::Started)
        }

        fn record(
            &self,
            operation: &BackendOperationId,
            evidence: OperationEvidence,
        ) -> polis::Result<()> {
            self.operation.borrow_mut().push(operation.clone());
            self.evidence.borrow_mut().push(evidence.kind);
            Ok(())
        }

        fn close(
            &self,
            operation: &BackendOperationId,
            marker: TerminalMarker,
        ) -> polis::Result<()> {
            self.operation.borrow_mut().push(operation.clone());
            self.terminal.borrow_mut().push(marker);
            if let Some(error) = &self.close_error {
                return Err(error.clone());
            }
            Ok(())
        }
    }

    enum TestCommand {}

    #[test]
    fn command_runner_terminalizes_success_once() {
        let operations = FakeOperations::default();
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result = runner.run(envelope, |context| {
            assert_eq!(
                context.mutation().authority().epoch(),
                AuthorityEpoch::new(1)
            );
            Ok::<_, PrimitiveFailure>(7)
        });

        assert_eq!(result, Ok(7));
        assert_eq!(
            operations.terminal.borrow().as_slice(),
            [TerminalMarker::Succeeded]
        );
        assert!(operations.evidence.borrow().is_empty());
    }

    #[test]
    fn command_runner_replay_does_not_run_work_or_write_terminal() {
        let operations = FakeOperations {
            replay: Some(None),
            ..FakeOperations::default()
        };
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result: Result<(), PrimitiveFailure> = runner.run(envelope, |_context| {
            panic!("replayed commands should not run product work")
        });

        assert_eq!(result, Err(PrimitiveFailure::OperationInProgress));
        assert!(operations.evidence.borrow().is_empty());
        assert!(operations.terminal.borrow().is_empty());
    }

    #[test]
    fn command_runner_replay_uses_product_verifier_without_writing_terminal() {
        let operations = FakeOperations {
            replay: Some(Some(TerminalMarker::Succeeded)),
            ..FakeOperations::default()
        };
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result = runner.run_with_replay(
            envelope,
            |_context| panic!("replayed commands should not run product work"),
            |mutation| {
                assert_eq!(mutation.authority().epoch(), AuthorityEpoch::new(1));
                Ok::<_, PrimitiveFailure>(9)
            },
        );

        assert_eq!(result, Ok(9));
        assert!(operations.evidence.borrow().is_empty());
        assert!(operations.terminal.borrow().is_empty());
    }

    #[test]
    fn command_runner_non_success_replay_returns_explicit_state_without_verifying() {
        for (terminal, expected) in [
            (None, PrimitiveFailure::OperationInProgress),
            (
                Some(TerminalMarker::Failed(Vec::new())),
                PrimitiveFailure::OperationAlreadyFailed,
            ),
            (
                Some(TerminalMarker::Interrupted),
                PrimitiveFailure::OperationInterrupted,
            ),
        ] {
            let operations = FakeOperations {
                replay: Some(terminal),
                ..FakeOperations::default()
            };
            let runner = AttemptLog::new(operations.clone());
            let envelope: IssuedAttempt<TestCommand> =
                IssuedAttempt::new(context(), operation_request());

            let result: Result<(), PrimitiveFailure> = runner.run_with_replay(
                envelope,
                |_context| panic!("replayed commands should not run product work"),
                |_mutation| panic!("only terminal success replay should verify"),
            );

            assert_eq!(result, Err(expected));
            assert!(operations.evidence.borrow().is_empty());
            assert!(operations.terminal.borrow().is_empty());
        }
    }

    #[test]
    fn command_runner_terminalizes_failure_once() {
        let operations = FakeOperations::default();
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result = runner.run(envelope, |_context| {
            Err::<(), _>(PrimitiveFailure::Conflict)
        });

        assert_eq!(result, Err(PrimitiveFailure::Conflict));
        assert_eq!(
            operations.terminal.borrow().as_slice(),
            [TerminalMarker::Failed(Vec::new())]
        );
        assert!(operations.evidence.borrow().is_empty());
    }

    #[test]
    fn command_runner_can_terminalize_failure_as_interrupted() {
        let operations = FakeOperations::default();
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result = runner.run_with_replay_and_failure_disposition(
            envelope,
            AttemptFailureDisposition::Interrupted,
            |_context| Err::<(), _>(PrimitiveFailure::Timeout),
            |_mutation| panic!("fresh work should not replay"),
        );

        assert_eq!(result, Err(PrimitiveFailure::Timeout));
        assert_eq!(
            operations.terminal.borrow().as_slice(),
            [TerminalMarker::Interrupted]
        );
    }

    #[test]
    fn attempt_log_returns_terminalization_failure_when_failed_close_fails() {
        let operations = FakeOperations {
            close_error: Some(polis::Error::TerminalAlreadyWritten),
            ..FakeOperations::default()
        };
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result = runner.run(envelope, |_context| {
            Err::<(), _>(PrimitiveFailure::Conflict)
        });

        assert_eq!(result, Err(PrimitiveFailure::TerminalAlreadyWritten));
        assert_eq!(
            operations.terminal.borrow().as_slice(),
            [TerminalMarker::Failed(Vec::new())]
        );
    }

    #[test]
    fn command_runner_returns_success_close_failure_when_work_succeeds() {
        let operations = FakeOperations {
            close_error: Some(polis::Error::TerminalAlreadyWritten),
            ..FakeOperations::default()
        };
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result = runner.run(envelope, |_context| Ok::<_, PrimitiveFailure>(7));

        assert_eq!(result, Err(PrimitiveFailure::TerminalAlreadyWritten));
        assert_eq!(
            operations.terminal.borrow().as_slice(),
            [TerminalMarker::Succeeded]
        );
    }

    #[test]
    fn command_context_records_explicit_checkpoints_only() {
        let operations = FakeOperations::default();
        let runner = AttemptLog::new(operations.clone());

        let envelope: IssuedAttempt<TestCommand> =
            IssuedAttempt::new(context(), operation_request());
        let result = runner.run(envelope, |context| {
            context.checkpoint(AttemptCheckpoint::new("test.progress"))?;
            Ok::<_, PrimitiveFailure>(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(
            operations.evidence.borrow().as_slice(),
            [EvidenceKind::Checkpoint(b"test.progress;".to_vec())]
        );
    }

    #[test]
    fn command_checkpoint_encodes_fields_with_lengths() {
        let checkpoint = AttemptCheckpoint::new("test.progress")
            .field("resource", "db;owner=wrong")
            .field("holder", "node=b");

        assert_eq!(
            checkpoint.encode(),
            b"test.progress;resource=14:db;owner=wrong;holder=6:node=b;".to_vec()
        );
    }

    fn context() -> MutationContext {
        let operation = OperationId::parse("op-1").expect("operation");
        let idempotency = IdempotencyKey::parse("idem-1").expect("idempotency");
        MutationContext::new(
            operation,
            idempotency,
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(1),
            ),
            None,
            SystemTime::UNIX_EPOCH,
        )
    }

    fn operation_request() -> OperationRequest {
        let fingerprint = polis::RequestFingerprint::new(
            polis::PrincipalId::parse("node-a").expect("principal"),
            polis::ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("test").expect("command"),
            vec![1],
            vec![FingerprintedResource::parse("resource:test").expect("resource")],
            None,
            polis::GrantEpoch::new(1),
        )
        .expect("fingerprint");
        OperationRequest::new(
            polis::OperationId::parse("op-1").expect("operation"),
            polis::IdempotencyKey::parse("idem-1").expect("idempotency"),
            fingerprint,
            SystemTime::UNIX_EPOCH,
        )
    }
}
