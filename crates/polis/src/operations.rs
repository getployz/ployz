//! Operation evidence and idempotency primitives.

use std::time::SystemTime;

use crate::authority::GrantEpoch;
use crate::identity::{PrincipalId, ScopeId};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestFingerprint {
    actor: PrincipalId,
    scope: ScopeId,
    command: CommandKind,
    payload_hash: Vec<u8>,
    resources: Vec<FingerprintedResource>,
    authority_epoch: GrantEpoch,
}

impl RequestFingerprint {
    pub fn new(
        actor: PrincipalId,
        scope: ScopeId,
        command: CommandKind,
        payload_hash: Vec<u8>,
        resources: Vec<FingerprintedResource>,
        authority_epoch: GrantEpoch,
    ) -> Result<Self> {
        if payload_hash.is_empty() || resources.is_empty() {
            return Err(Error::MalformedPayload);
        }
        let mut resources = resources;
        resources.sort();
        resources.dedup();
        Ok(Self {
            actor,
            scope,
            command,
            payload_hash,
            resources,
            authority_epoch,
        })
    }

    #[must_use]
    pub fn actor(&self) -> &PrincipalId {
        &self.actor
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub fn command(&self) -> &CommandKind {
        &self.command
    }

    #[must_use]
    pub fn payload_hash(&self) -> &[u8] {
        &self.payload_hash
    }

    #[must_use]
    pub fn resources(&self) -> &[FingerprintedResource] {
        &self.resources
    }

    #[must_use]
    pub fn authority_epoch(&self) -> GrantEpoch {
        self.authority_epoch
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandKind(String);

impl CommandKind {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FingerprintedResource(String);

impl FingerprintedResource {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRequest {
    operation: OperationId,
    idempotency: IdempotencyKey,
    fingerprint: RequestFingerprint,
    owner_deadline: SystemTime,
}

impl OperationRequest {
    #[must_use]
    pub fn new(
        operation: OperationId,
        idempotency: IdempotencyKey,
        fingerprint: RequestFingerprint,
        owner_deadline: SystemTime,
    ) -> Self {
        Self {
            operation,
            idempotency,
            fingerprint,
            owner_deadline,
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
    pub fn fingerprint(&self) -> &RequestFingerprint {
        &self.fingerprint
    }

    #[must_use]
    pub fn owner_deadline(&self) -> SystemTime {
        self.owner_deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStart {
    Started(OpenOperation),
    Replayed(OperationReplay),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendOperationStart {
    Started,
    /// Existing operation found by a trusted durable backend after validating
    /// the idempotency key and request fingerprint.
    Replayed {
        operation: OperationId,
        terminal: Option<TerminalMarker>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOperation {
    operation: OperationId,
    idempotency: IdempotencyKey,
    fingerprint: RequestFingerprint,
}

impl OpenOperation {
    #[must_use]
    fn from_request(request: &OperationRequest) -> Self {
        Self {
            operation: request.operation.clone(),
            idempotency: request.idempotency.clone(),
            fingerprint: request.fingerprint.clone(),
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
    pub fn fingerprint(&self) -> &RequestFingerprint {
        &self.fingerprint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationReplay {
    operation: OperationId,
    terminal: Option<TerminalMarker>,
}

impl OperationReplay {
    #[must_use]
    fn from_backend(operation: OperationId, terminal: Option<TerminalMarker>) -> Self {
        Self {
            operation,
            terminal,
        }
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn terminal(&self) -> Option<&TerminalMarker> {
        self.terminal.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationEvidence {
    pub recorded_at: SystemTime,
    pub kind: EvidenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceKind {
    Checkpoint(Vec<u8>),
    Observation(Vec<u8>),
    Failure(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMarker {
    Succeeded,
    Failed(Vec<u8>),
    Interrupted,
}

/// Trusted durable operation state.
///
/// Implementations must validate idempotency keys against the full request
/// fingerprint before returning a replay decision. Polis uses that backend
/// decision to mint the lifecycle proof values exposed to callers.
pub trait OperationBackend {
    fn start_or_replay(&self, request: &OperationRequest) -> Result<BackendOperationStart>;

    fn record(&self, operation: &OperationId, evidence: OperationEvidence) -> Result<()>;

    fn close(&self, operation: &OperationId, marker: TerminalMarker) -> Result<()>;
}

pub fn start_or_replay(
    backend: &dyn OperationBackend,
    request: OperationRequest,
) -> Result<OperationStart> {
    match backend.start_or_replay(&request)? {
        BackendOperationStart::Started => Ok(OperationStart::Started(OpenOperation::from_request(
            &request,
        ))),
        BackendOperationStart::Replayed {
            operation,
            terminal,
        } => Ok(OperationStart::Replayed(OperationReplay::from_backend(
            operation, terminal,
        ))),
    }
}

pub fn record(
    backend: &dyn OperationBackend,
    operation: &OpenOperation,
    evidence: OperationEvidence,
) -> Result<()> {
    backend.record(operation.operation(), evidence)
}

pub fn close(
    backend: &dyn OperationBackend,
    operation: OpenOperation,
    marker: TerminalMarker,
) -> Result<()> {
    backend.close(operation.operation(), marker)
}

fn parse_non_empty<T>(value: impl Into<String>, build: impl FnOnce(String) -> T) -> Result<T> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(Error::MalformedPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    struct MemoryOperationStore {
        by_idempotency: RefCell<BTreeMap<IdempotencyKey, OpenOperation>>,
        terminal: RefCell<BTreeMap<OperationId, TerminalMarker>>,
    }

    impl MemoryOperationStore {
        fn new() -> Self {
            Self {
                by_idempotency: RefCell::new(BTreeMap::new()),
                terminal: RefCell::new(BTreeMap::new()),
            }
        }
    }

    impl OperationBackend for MemoryOperationStore {
        fn start_or_replay(&self, request: &OperationRequest) -> Result<BackendOperationStart> {
            let by_idempotency = self.by_idempotency.borrow();
            if let Some(existing) = by_idempotency.get(request.idempotency()) {
                if existing.fingerprint() != request.fingerprint() {
                    return Err(Error::Conflict);
                }
                return Ok(BackendOperationStart::Replayed {
                    operation: existing.operation().clone(),
                    terminal: self.terminal.borrow().get(existing.operation()).cloned(),
                });
            }
            drop(by_idempotency);
            let record = OpenOperation::from_request(request);
            self.by_idempotency
                .borrow_mut()
                .insert(record.idempotency().clone(), record.clone());
            Ok(BackendOperationStart::Started)
        }

        fn record(&self, _operation: &OperationId, _evidence: OperationEvidence) -> Result<()> {
            Ok(())
        }

        fn close(&self, operation: &OperationId, marker: TerminalMarker) -> Result<()> {
            let mut terminal = self.terminal.borrow_mut();
            if terminal.contains_key(operation) {
                return Err(Error::TerminalAlreadyWritten);
            }
            terminal.insert(operation.clone(), marker);
            Ok(())
        }
    }

    fn fingerprint(payload_hash: &[u8], authority_epoch: u64) -> RequestFingerprint {
        RequestFingerprint::new(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("deploy").expect("command"),
            payload_hash.to_vec(),
            vec![
                FingerprintedResource::parse("route:app").expect("route"),
                FingerprintedResource::parse("cert:app.example.com").expect("cert"),
            ],
            GrantEpoch::new(authority_epoch),
        )
        .expect("fingerprint")
    }

    fn request(payload_hash: &[u8], authority_epoch: u64) -> OperationRequest {
        OperationRequest::new(
            OperationId::parse("op-1").expect("operation id"),
            IdempotencyKey::parse("deploy-1").expect("idempotency key"),
            fingerprint(payload_hash, authority_epoch),
            UNIX_EPOCH + Duration::from_secs(10),
        )
    }

    #[test]
    fn same_idempotency_and_fingerprint_replays_record() {
        let store = MemoryOperationStore::new();
        let _started = start_or_replay(&store, request(&[1, 2, 3], 7)).expect("started");

        let replayed = start_or_replay(&store, request(&[1, 2, 3], 7)).expect("replayed");

        assert!(matches!(replayed, OperationStart::Replayed(_)));
    }

    #[test]
    fn same_idempotency_with_different_fingerprint_conflicts() {
        let store = MemoryOperationStore::new();
        let _started = start_or_replay(&store, request(&[1, 2, 3], 7)).expect("started");

        assert_eq!(
            start_or_replay(&store, request(&[9], 7)),
            Err(Error::Conflict)
        );
    }

    #[test]
    fn same_payload_with_different_authority_epoch_conflicts() {
        let store = MemoryOperationStore::new();
        let _started = start_or_replay(&store, request(&[1, 2, 3], 7)).expect("started");

        assert_eq!(
            start_or_replay(&store, request(&[1, 2, 3], 8)),
            Err(Error::Conflict)
        );
    }

    #[test]
    fn closing_an_operation_twice_is_rejected() {
        let store = MemoryOperationStore::new();
        let OperationStart::Started(open) =
            start_or_replay(&store, request(&[1, 2, 3], 7)).expect("started")
        else {
            panic!("expected started operation");
        };

        close(&store, open.clone(), TerminalMarker::Succeeded).expect("closed");

        assert_eq!(
            close(&store, open, TerminalMarker::Succeeded),
            Err(Error::TerminalAlreadyWritten)
        );
    }
}
