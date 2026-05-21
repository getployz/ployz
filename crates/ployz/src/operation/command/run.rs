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

    pub(crate) fn checkpoint(&self, checkpoint: CommandCheckpoint) -> Result<(), PrimitiveFailure> {
        record_operation(
            self.operations,
            &self.operation,
            EvidenceKind::Checkpoint(checkpoint.encode()),
        )
        .map_err(map_polis_to_primitive)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCheckpoint {
    name: &'static str,
    fields: Vec<CheckpointField>,
}

impl CommandCheckpoint {
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

pub trait CommandBackend {
    fn run<C, T, E, F>(
        &self,
        envelope: CommandEnvelope<C>,
        map_primitive: fn(PrimitiveFailure) -> E,
        work: F,
    ) -> Result<T, E>
    where
        F: FnOnce(&CommandContext<'_>) -> Result<T, E>;

    fn run_with_replay<C, T, E, F, R>(
        &self,
        envelope: CommandEnvelope<C>,
        map_primitive: fn(PrimitiveFailure) -> E,
        work: F,
        verify_replayed_success: R,
    ) -> Result<T, E>
    where
        F: FnOnce(&CommandContext<'_>) -> Result<T, E>,
        R: FnOnce(&MutationContext) -> Result<T, E>;
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
        self.run_with_replay(envelope, map_primitive, work, |_replay| {
            Err(map_primitive(PrimitiveFailure::ReplayUnavailable))
        })
    }

    fn run_with_replay<C, T, E, F, R>(
        &self,
        envelope: CommandEnvelope<C>,
        map_primitive: fn(PrimitiveFailure) -> E,
        work: F,
        verify_replayed_success: R,
    ) -> Result<T, E>
    where
        F: FnOnce(&CommandContext<'_>) -> Result<T, E>,
        R: FnOnce(&MutationContext) -> Result<T, E>,
    {
        let mutation = envelope.context().clone();
        let start = polis::start_or_replay(&self.operations, envelope.operation_request)
            .map_err(map_polis_to_primitive)
            .map_err(&map_primitive)?;

        let operation = match start {
            polis::OperationStart::Started(operation) => operation,
            polis::OperationStart::Replayed(replay) => {
                if replay.terminal() == Some(&TerminalMarker::Succeeded) {
                    return verify_replayed_success(&mutation);
                }
                return Err(map_primitive(PrimitiveFailure::ReplayUnavailable));
            }
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
        replay: Option<Option<TerminalMarker>>,
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
            Ok(())
        }
    }

    enum TestCommand {}

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
            replay: Some(None),
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
    fn command_runner_replay_uses_product_verifier_without_writing_terminal() {
        let operations = FakeOperations {
            replay: Some(Some(TerminalMarker::Succeeded)),
            ..FakeOperations::default()
        };
        let runner = CommandRunner::new(operations.clone());

        let envelope: CommandEnvelope<TestCommand> =
            CommandEnvelope::new(context(), operation_request());
        let result = runner.run_with_replay(
            envelope,
            |failure| failure,
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
    fn command_runner_non_success_replay_never_calls_product_verifier() {
        for terminal in [
            None,
            Some(TerminalMarker::Failed(Vec::new())),
            Some(TerminalMarker::Interrupted),
        ] {
            let operations = FakeOperations {
                replay: Some(terminal),
                ..FakeOperations::default()
            };
            let runner = CommandRunner::new(operations.clone());
            let envelope: CommandEnvelope<TestCommand> =
                CommandEnvelope::new(context(), operation_request());

            let result: Result<(), PrimitiveFailure> = runner.run_with_replay(
                envelope,
                |failure| failure,
                |_context| panic!("replayed commands should not run product work"),
                |_mutation| panic!("only terminal success replay should verify"),
            );

            assert_eq!(result, Err(PrimitiveFailure::ReplayUnavailable));
            assert!(operations.evidence.borrow().is_empty());
            assert!(operations.terminal.borrow().is_empty());
        }
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
                context.checkpoint(CommandCheckpoint::new("test.progress"))?;
                Ok::<_, PrimitiveFailure>(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            operations.evidence.borrow().as_slice(),
            [EvidenceKind::Checkpoint(b"test.progress;".to_vec())]
        );
    }

    #[test]
    fn command_checkpoint_encodes_fields_with_lengths() {
        let checkpoint = CommandCheckpoint::new("test.progress")
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
