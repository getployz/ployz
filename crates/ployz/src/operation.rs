//! Product-neutral operation context shared by Ployz domains.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::time::SystemTime;

use crate::error::PrimitiveFailure;

pub use polis::{
    BackendOperationStart, CommandKind, EvidenceKind, FingerprintedResource, OperationBackend,
    OperationEvidence, OperationId as BackendOperationId, OperationRequest, TerminalMarker,
};

pub type OperationResult<T> = polis::Result<T>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrincipalId(String);

impl PrincipalId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(String);

impl ScopeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityEpoch(u64);

impl AuthorityEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityContext {
    principal: PrincipalId,
    scope: ScopeId,
    epoch: AuthorityEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorityDecision {
    Allowed(AuthorityEpoch),
    Denied,
    Unknown,
}

impl AuthorityContext {
    #[must_use]
    pub(crate) fn new(principal: PrincipalId, scope: ScopeId, epoch: AuthorityEpoch) -> Self {
        Self {
            principal,
            scope,
            epoch,
        }
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub fn epoch(&self) -> AuthorityEpoch {
        self.epoch
    }
}

impl AuthorityDecision {
    #[must_use]
    pub fn epoch(&self) -> Option<AuthorityEpoch> {
        let Self::Allowed(epoch) = self else {
            return None;
        };
        Some(*epoch)
    }
}

pub trait AuthorityPort {
    fn decide(
        &self,
        principal: &PrincipalId,
        scope: &ScopeId,
    ) -> Result<AuthorityDecision, PrimitiveFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct TypedResourceId<R> {
    value: String,
    _resource: PhantomData<fn() -> R>,
}

impl<R> TypedResourceId<R> {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self {
            value,
            _resource: PhantomData,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl<R> Clone for TypedResourceId<R> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            _resource: PhantomData,
        }
    }
}

impl<R> PartialEq for TypedResourceId<R> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<R> Eq for TypedResourceId<R> {}

impl<R> PartialOrd for TypedResourceId<R> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<R> Ord for TypedResourceId<R> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

impl<R> Hash for TypedResourceId<R> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FenceEpoch(u64);

impl FenceEpoch {
    pub fn new(value: u64) -> Result<Self, PrimitiveFailure> {
        if value == 0 || value == u64::MAX {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimHash(String);

impl ClaimHash {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedFenceToken {
    pub resource: ResourceId,
    pub holder: PrincipalId,
    pub epoch: FenceEpoch,
    pub claim_hash: ClaimHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimGuard<R> {
    resource: TypedResourceId<R>,
    holder: PrincipalId,
    epoch: FenceEpoch,
    claim_hash: ClaimHash,
    expires_at: SystemTime,
}

impl<R> ClaimGuard<R> {
    #[must_use]
    pub(crate) fn new(
        resource: TypedResourceId<R>,
        holder: PrincipalId,
        epoch: FenceEpoch,
        claim_hash: ClaimHash,
        expires_at: SystemTime,
    ) -> Self {
        Self {
            resource,
            holder,
            epoch,
            claim_hash,
            expires_at,
        }
    }

    #[must_use]
    pub fn resource(&self) -> &TypedResourceId<R> {
        &self.resource
    }

    #[must_use]
    pub fn holder(&self) -> &PrincipalId {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> FenceEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> &ClaimHash {
        &self.claim_hash
    }

    #[must_use]
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    operation: OperationId,
    idempotency: IdempotencyKey,
    authority: AuthorityContext,
    submitted_fence: Option<SubmittedFenceToken>,
    deadline: SystemTime,
}

impl MutationContext {
    #[must_use]
    pub(crate) fn new(
        operation: OperationId,
        idempotency: IdempotencyKey,
        authority: AuthorityContext,
        submitted_fence: Option<SubmittedFenceToken>,
        deadline: SystemTime,
    ) -> Self {
        Self {
            operation,
            idempotency,
            authority,
            submitted_fence,
            deadline,
        }
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }

    #[must_use]
    pub fn authority(&self) -> &AuthorityContext {
        &self.authority
    }

    #[must_use]
    pub fn submitted_fence(&self) -> Option<&SubmittedFenceToken> {
        self.submitted_fence.as_ref()
    }

    #[must_use]
    pub fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationIntent {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub command: CommandKind,
    pub payload_hash: Vec<u8>,
    pub resources: Vec<FingerprintedResource>,
    pub submitted_fence: Option<SubmittedFenceToken>,
    pub deadline: SystemTime,
}

pub struct CommandIssuer<A> {
    authority: A,
}

impl<A> CommandIssuer<A> {
    #[must_use]
    pub fn new(authority: A) -> Self {
        Self { authority }
    }
}

impl<A> CommandIssuer<A>
where
    A: AuthorityPort,
{
    pub fn issue<C>(&self, intent: MutationIntent) -> Result<CommandEnvelope<C>, PrimitiveFailure> {
        let epoch = match self
            .authority
            .decide(&intent.principal, &intent.scope)?
            .epoch()
        {
            Some(epoch) => epoch,
            None => return Err(PrimitiveFailure::Unauthorized),
        };
        let operation =
            polis::OperationId::parse(intent.operation.as_str()).map_err(map_polis_to_primitive)?;
        let idempotency = polis::IdempotencyKey::parse(intent.idempotency.as_str())
            .map_err(map_polis_to_primitive)?;
        let actor =
            polis::PrincipalId::parse(intent.principal.as_str()).map_err(map_polis_to_primitive)?;
        let scope = polis::ScopeId::parse(intent.scope.as_str()).map_err(map_polis_to_primitive)?;
        let fingerprint = polis::RequestFingerprint::new(
            actor,
            scope,
            intent.command,
            intent.payload_hash,
            intent.resources,
            polis::GrantEpoch::new(epoch.value()),
        )
        .map_err(map_polis_to_primitive)?;
        let operation_request =
            OperationRequest::new(operation, idempotency, fingerprint, intent.deadline);
        let context = MutationContext::new(
            intent.operation,
            intent.idempotency,
            AuthorityContext::new(intent.principal, intent.scope, epoch),
            intent.submitted_fence,
            intent.deadline,
        );
        Ok(CommandEnvelope::new(context, operation_request))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEnvelope<C = ()> {
    context: MutationContext,
    operation_request: OperationRequest,
    _command: PhantomData<fn() -> C>,
}

impl<C> CommandEnvelope<C> {
    #[must_use]
    pub(crate) fn new(context: MutationContext, operation_request: OperationRequest) -> Self {
        Self {
            context,
            operation_request,
            _command: PhantomData,
        }
    }

    #[must_use]
    pub fn context(&self) -> &MutationContext {
        &self.context
    }
}

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

    pub fn checkpoint(&self) -> Result<(), PrimitiveFailure> {
        record_operation(
            self.operations,
            &self.operation,
            EvidenceKind::Checkpoint(Vec::new()),
        )
        .map_err(map_polis_to_primitive)
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

        record_operation(
            &self.operations,
            &operation,
            EvidenceKind::Observation(Vec::new()),
        )
        .map_err(map_polis_to_primitive)
        .map_err(&map_primitive)?;

        let context = CommandContext::new(mutation, operation, &self.operations);

        match work(&context) {
            Ok(value) => {
                context.checkpoint().map_err(&map_primitive)?;
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
                record_operation(
                    &self.operations,
                    &context.operation,
                    EvidenceKind::Failure(Vec::new()),
                )
                .map_err(map_polis_to_primitive)
                .map_err(&map_primitive)?;
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

fn map_polis_to_primitive(error: polis::Error) -> PrimitiveFailure {
    match error {
        polis::Error::Unauthorized => PrimitiveFailure::Unauthorized,
        polis::Error::Conflict => PrimitiveFailure::Conflict,
        polis::Error::Timeout => PrimitiveFailure::Timeout,
        polis::Error::StaleFence => PrimitiveFailure::StaleFence,
        polis::Error::NoResponder => PrimitiveFailure::NoResponder,
        polis::Error::FreshnessUnknown => PrimitiveFailure::FreshnessUnknown,
        polis::Error::MalformedPayload => PrimitiveFailure::MalformedPayload,
        polis::Error::TerminalAlreadyWritten => PrimitiveFailure::TerminalAlreadyWritten,
    }
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, PrimitiveFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(PrimitiveFailure::MalformedPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_operation_id_is_malformed_payload() {
        assert_eq!(
            OperationId::parse(""),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[test]
    fn invalid_fence_epochs_are_malformed_payload() {
        assert_eq!(FenceEpoch::new(0), Err(PrimitiveFailure::MalformedPayload));
        assert_eq!(
            FenceEpoch::new(u64::MAX),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[derive(Clone, Default)]
    struct FakeOperations {
        evidence: std::rc::Rc<std::cell::RefCell<Vec<EvidenceKind>>>,
        terminal: std::rc::Rc<std::cell::RefCell<Vec<TerminalMarker>>>,
        operation: std::rc::Rc<std::cell::RefCell<Vec<BackendOperationId>>>,
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
    }

    #[test]
    fn command_issuer_builds_command_envelope_from_allowed_authority() {
        struct AllowAuthority;

        impl AuthorityPort for AllowAuthority {
            fn decide(
                &self,
                _principal: &PrincipalId,
                _scope: &ScopeId,
            ) -> Result<AuthorityDecision, PrimitiveFailure> {
                Ok(AuthorityDecision::Allowed(AuthorityEpoch::new(7)))
            }
        }

        let envelope = CommandIssuer::new(AllowAuthority)
            .issue::<TestCommand>(MutationIntent {
                operation: OperationId::parse("op-1").expect("operation"),
                idempotency: IdempotencyKey::parse("idem-1").expect("idempotency"),
                principal: PrincipalId::parse("node-a").expect("principal"),
                scope: ScopeId::parse("cluster").expect("scope"),
                command: CommandKind::parse("test").expect("command"),
                payload_hash: vec![1],
                resources: vec![FingerprintedResource::parse("resource:test").expect("resource")],
                submitted_fence: None,
                deadline: SystemTime::UNIX_EPOCH,
            })
            .expect("envelope");

        assert_eq!(
            envelope.context().authority().epoch(),
            AuthorityEpoch::new(7)
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
