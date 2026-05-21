use std::time::SystemTime;

use polis::{EvidenceKind, OperationBackend, OperationEvidence, TerminalMarker};

use crate::error::PrimitiveFailure;
use crate::operation::command::issue::CommandEnvelope;
use crate::operation::context::MutationContext;
use crate::operation::polis_boundary::map_polis_to_primitive;

pub struct CommandContext<'a> {
    mutation: MutationContext,
    operation: polis::OpenOperation,
    operations: &'a dyn OperationBackend,
}

impl<'a> CommandContext<'a> {
    #[must_use]
    pub(crate) fn new(
        mutation: MutationContext,
        operation: polis::OpenOperation,
        operations: &'a dyn OperationBackend,
    ) -> Self {
        Self {
            mutation,
            operation,
            operations,
        }
    }

    #[must_use]
    pub fn mutation(&self) -> &MutationContext {
        &self.mutation
    }

    pub(crate) fn checkpoint(
        &self,
        checkpoint: impl CommandCheckpoint,
    ) -> Result<(), PrimitiveFailure> {
        record_operation(
            self.operations,
            &self.operation,
            EvidenceKind::Checkpoint(checkpoint.encode_checkpoint()),
        )
        .map_err(map_polis_to_primitive)
    }
}

pub(crate) trait CommandCheckpoint {
    fn encode_checkpoint(&self) -> Vec<u8>;
}

pub trait CommandBackend {
    fn run<C, T, E, F>(
        &self,
        envelope: CommandEnvelope<C>,
        map_primitive: fn(PrimitiveFailure) -> E,
        work: F,
    ) -> Result<T, E>
    where
        F: FnOnce(&CommandContext<'_>) -> Result<T, E>;
}

pub struct CommandRunner<O> {
    operations: O,
}

impl<O> CommandRunner<O> {
    #[must_use]
    pub fn new(operations: O) -> Self {
        Self { operations }
    }
}

impl<O> CommandBackend for CommandRunner<O>
where
    O: OperationBackend,
{
    fn run<C, T, E, F>(
        &self,
        envelope: CommandEnvelope<C>,
        map_primitive: fn(PrimitiveFailure) -> E,
        work: F,
    ) -> Result<T, E>
    where
        F: FnOnce(&CommandContext<'_>) -> Result<T, E>,
    {
        let mutation = envelope.context().clone();
        let start = polis::start_or_replay(&self.operations, envelope.operation_request)
            .map_err(map_polis_to_primitive)
            .map_err(&map_primitive)?;

        let polis::OperationStart::Started(operation) = start else {
            return Err(map_primitive(PrimitiveFailure::ReplayUnavailable));
        };

        let context = CommandContext::new(mutation, operation, &self.operations);

        match work(&context) {
            Ok(value) => {
                polis::close(
                    &self.operations,
                    context.operation,
                    TerminalMarker::Succeeded,
                )
                .map_err(map_polis_to_primitive)
                .map_err(map_primitive)?;
                Ok(value)
            }
            Err(error) => {
                polis::close(
                    &self.operations,
                    context.operation,
                    TerminalMarker::Failed(Vec::new()),
                )
                .map_err(map_polis_to_primitive)
                .map_err(map_primitive)?;
                Err(error)
            }
        }
    }
}

fn record_operation(
    operations: &dyn OperationBackend,
    operation: &polis::OpenOperation,
    kind: EvidenceKind,
) -> polis::Result<()> {
    polis::record(
        operations,
        operation,
        OperationEvidence {
            recorded_at: SystemTime::now(),
            kind,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use polis::{
        BackendOperationStart, CommandKind, FingerprintedResource,
        OperationId as BackendOperationId, OperationRequest,
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
        replay: bool,
    }

    impl OperationBackend for FakeOperations {
        fn start_or_replay(
            &self,
            request: &OperationRequest,
        ) -> polis::Result<BackendOperationStart> {
            if self.replay {
                return Ok(BackendOperationStart::Replayed {
                    operation: request.operation().clone(),
                    terminal: None,
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
            Ok(())
        }
    }

    enum TestCommand {}

    enum TestCheckpoint {
        Progress,
    }

    impl CommandCheckpoint for TestCheckpoint {
        fn encode_checkpoint(&self) -> Vec<u8> {
            match self {
                Self::Progress => b"test.progress".to_vec(),
            }
        }
    }

    #[test]
    fn command_runner_terminalizes_success_once() {
        let operations = FakeOperations::default();
        let runner = CommandRunner::new(operations.clone());

        let envelope: CommandEnvelope<TestCommand> =
            CommandEnvelope::new(context(), operation_request());
        let result = runner.run(
            envelope,
            |failure| failure,
            |context| {
                assert_eq!(
                    context.mutation().authority().epoch(),
                    AuthorityEpoch::new(1)
                );
                Ok::<_, PrimitiveFailure>(7)
            },
        );

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
            replay: true,
            ..FakeOperations::default()
        };
        let runner = CommandRunner::new(operations.clone());

        let envelope: CommandEnvelope<TestCommand> =
            CommandEnvelope::new(context(), operation_request());
        let result: Result<(), PrimitiveFailure> = runner.run(
            envelope,
            |failure| failure,
            |_context| panic!("replayed commands should not run product work"),
        );

        assert_eq!(result, Err(PrimitiveFailure::ReplayUnavailable));
        assert!(operations.evidence.borrow().is_empty());
        assert!(operations.terminal.borrow().is_empty());
    }

    #[test]
    fn command_runner_terminalizes_failure_once() {
        let operations = FakeOperations::default();
        let runner = CommandRunner::new(operations.clone());

        let envelope: CommandEnvelope<TestCommand> =
            CommandEnvelope::new(context(), operation_request());
        let result = runner.run(
            envelope,
            |failure| failure,
            |_context| Err::<(), _>(PrimitiveFailure::Conflict),
        );

        assert_eq!(result, Err(PrimitiveFailure::Conflict));
        assert_eq!(
            operations.terminal.borrow().as_slice(),
            [TerminalMarker::Failed(Vec::new())]
        );
        assert!(operations.evidence.borrow().is_empty());
    }

    #[test]
    fn command_context_records_explicit_checkpoints_only() {
        let operations = FakeOperations::default();
        let runner = CommandRunner::new(operations.clone());

        let envelope: CommandEnvelope<TestCommand> =
            CommandEnvelope::new(context(), operation_request());
        let result = runner.run(
            envelope,
            |failure| failure,
            |context| {
                context.checkpoint(TestCheckpoint::Progress)?;
                Ok::<_, PrimitiveFailure>(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            operations.evidence.borrow().as_slice(),
            [EvidenceKind::Checkpoint(b"test.progress".to_vec())]
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
