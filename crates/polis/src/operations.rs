//! Operation evidence and idempotency primitives.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::authority::GrantEpoch;
use crate::identity::{PrincipalId, ScopeId};
use crate::{Error, Result};
use sha2::{Digest, Sha256};

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
    request_hash: Vec<u8>,
    resources: Vec<FingerprintedResource>,
    submitted_fence: Option<Box<SubmittedFenceFingerprint>>,
    authority_epoch: GrantEpoch,
}

impl RequestFingerprint {
    #[must_use]
    pub fn builder(
        actor: PrincipalId,
        scope: ScopeId,
        command: CommandKind,
        schema: impl Into<String>,
        authority_epoch: GrantEpoch,
    ) -> RequestFingerprintBuilder {
        RequestFingerprintBuilder::new(actor, scope, command, schema, authority_epoch)
    }

    pub fn new(
        actor: PrincipalId,
        scope: ScopeId,
        command: CommandKind,
        payload_hash: Vec<u8>,
        resources: Vec<FingerprintedResource>,
        submitted_fence: Option<SubmittedFenceFingerprint>,
        authority_epoch: GrantEpoch,
    ) -> Result<Self> {
        if payload_hash.is_empty() || resources.is_empty() {
            return Err(Error::MalformedPayload);
        }
        let mut resources = resources;
        resources.sort();
        resources.dedup();
        let request_hash = canonical_request_digest(
            &actor,
            &scope,
            &command,
            &payload_hash,
            &resources,
            submitted_fence.as_ref(),
            authority_epoch,
        );
        Ok(Self {
            actor,
            scope,
            command,
            payload_hash,
            request_hash,
            resources,
            submitted_fence: submitted_fence.map(Box::new),
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
    pub fn request_hash(&self) -> &[u8] {
        &self.request_hash
    }

    #[must_use]
    pub fn resources(&self) -> &[FingerprintedResource] {
        &self.resources
    }

    #[must_use]
    pub fn submitted_fence(&self) -> Option<&SubmittedFenceFingerprint> {
        self.submitted_fence.as_deref()
    }

    #[must_use]
    pub fn authority_epoch(&self) -> GrantEpoch {
        self.authority_epoch
    }
}

pub struct RequestFingerprintBuilder {
    actor: PrincipalId,
    scope: ScopeId,
    command: CommandKind,
    schema: String,
    fields: Vec<FingerprintField>,
    resources: Vec<FingerprintedResource>,
    submitted_fence: Option<SubmittedFenceFingerprint>,
    authority_epoch: GrantEpoch,
}

impl RequestFingerprintBuilder {
    #[must_use]
    fn new(
        actor: PrincipalId,
        scope: ScopeId,
        command: CommandKind,
        schema: impl Into<String>,
        authority_epoch: GrantEpoch,
    ) -> Self {
        Self {
            actor,
            scope,
            command,
            schema: schema.into(),
            fields: Vec::new(),
            resources: Vec::new(),
            submitted_fence: None,
            authority_epoch,
        }
    }

    #[must_use]
    pub fn field(mut self, key: &'static str, value: impl AsRef<str>) -> Self {
        self.fields.push(FingerprintField {
            key,
            value: FingerprintValue::String(value.as_ref().to_owned()),
        });
        self
    }

    #[must_use]
    pub fn field_u64(mut self, key: &'static str, value: u64) -> Self {
        self.fields.push(FingerprintField {
            key,
            value: FingerprintValue::U64(value),
        });
        self
    }

    #[must_use]
    pub fn field_time(mut self, key: &'static str, value: SystemTime) -> Self {
        self.fields.push(FingerprintField {
            key,
            value: FingerprintValue::Time(value),
        });
        self
    }

    #[must_use]
    pub fn resource(mut self, resource: FingerprintedResource) -> Self {
        self.resources.push(resource);
        self
    }

    #[must_use]
    pub fn submitted_fence(mut self, fence: SubmittedFenceFingerprint) -> Self {
        self.submitted_fence = Some(fence);
        self
    }

    pub fn finish(self) -> Result<RequestFingerprint> {
        if self.schema.trim().is_empty() || self.fields.is_empty() || self.resources.is_empty() {
            return Err(Error::MalformedPayload);
        }

        let payload_hash = canonical_payload_digest(&self.schema, self.fields)?;
        RequestFingerprint::new(
            self.actor,
            self.scope,
            self.command,
            payload_hash,
            self.resources,
            self.submitted_fence,
            self.authority_epoch,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FingerprintField {
    key: &'static str,
    value: FingerprintValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FingerprintValue {
    String(String),
    U64(u64),
    Time(SystemTime),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubmittedFenceFingerprint {
    resource: FingerprintedResource,
    holder: PrincipalId,
    epoch: u64,
    claim_hash: Vec<u8>,
}

impl SubmittedFenceFingerprint {
    pub fn new(
        resource: FingerprintedResource,
        holder: PrincipalId,
        epoch: u64,
        claim_hash: impl Into<Vec<u8>>,
    ) -> Result<Self> {
        let claim_hash = claim_hash.into();
        if claim_hash.is_empty() {
            return Err(Error::MalformedPayload);
        }
        if epoch == 0 || epoch == u64::MAX {
            return Err(Error::MalformedPayload);
        }
        Ok(Self {
            resource,
            holder,
            epoch,
            claim_hash,
        })
    }

    pub fn parse(
        resource: impl Into<String>,
        holder: impl Into<String>,
        epoch: u64,
        claim_hash: impl Into<Vec<u8>>,
    ) -> Result<Self> {
        let resource = FingerprintedResource::parse(resource)?;
        let holder = PrincipalId::parse(holder)?;
        Self::new(resource, holder, epoch, claim_hash)
    }

    #[must_use]
    pub fn resource(&self) -> &str {
        self.resource.as_str()
    }

    #[must_use]
    pub fn holder(&self) -> &str {
        self.holder.as_str()
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> &[u8] {
        &self.claim_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandKind(String);

impl CommandKind {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FingerprintedResource(String);

impl FingerprintedResource {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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
pub struct AttemptRequest {
    operation: OperationId,
    idempotency: IdempotencyKey,
    fingerprint: RequestFingerprint,
    owner_deadline: SystemTime,
}

impl AttemptRequest {
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

    #[must_use]
    fn as_operation_request(&self) -> OperationRequest {
        OperationRequest::new(
            self.operation.clone(),
            self.idempotency.clone(),
            self.fingerprint.clone(),
            self.owner_deadline,
        )
    }
}

impl From<OperationRequest> for AttemptRequest {
    fn from(request: OperationRequest) -> Self {
        Self::new(
            request.operation,
            request.idempotency,
            request.fingerprint,
            request.owner_deadline,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStart {
    Started(OpenOperation),
    Replayed(OperationReplay),
}

pub enum AttemptStart<'a> {
    Started(OpenAttempt<'a>),
    Replayed(AttemptReplay),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptReplay {
    Open {
        operation: OperationId,
        owner_deadline: SystemTime,
    },
    Succeeded {
        operation: OperationId,
    },
    Failed {
        operation: OperationId,
        payload: Vec<u8>,
    },
    Interrupted {
        operation: OperationId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptTerminal {
    Succeeded,
    Failed(Vec<u8>),
    Interrupted,
}

impl AttemptTerminal {
    #[must_use]
    fn marker(self) -> TerminalMarker {
        match self {
            Self::Succeeded => TerminalMarker::Succeeded,
            Self::Failed(payload) => TerminalMarker::Failed(payload),
            Self::Interrupted => TerminalMarker::Interrupted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendOperationStart {
    Started,
    /// Existing operation found by a trusted durable backend after validating
    /// the idempotency key and request fingerprint.
    Replayed {
        operation: OperationId,
        owner_deadline: SystemTime,
        terminal: Option<TerminalMarker>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOperation {
    operation: OperationId,
    idempotency: IdempotencyKey,
    fingerprint: RequestFingerprint,
    owner_deadline: SystemTime,
}

impl OpenOperation {
    #[must_use]
    fn from_request(request: &OperationRequest) -> Self {
        Self {
            operation: request.operation.clone(),
            idempotency: request.idempotency.clone(),
            fingerprint: request.fingerprint.clone(),
            owner_deadline: request.owner_deadline,
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

pub struct OpenAttempt<'a> {
    operation: OperationId,
    idempotency: IdempotencyKey,
    fingerprint: RequestFingerprint,
    backend: &'a dyn OperationBackend,
    terminalized: bool,
}

impl<'a> OpenAttempt<'a> {
    #[must_use]
    fn from_request(request: &OperationRequest, backend: &'a dyn OperationBackend) -> Self {
        Self {
            operation: request.operation.clone(),
            idempotency: request.idempotency.clone(),
            fingerprint: request.fingerprint.clone(),
            backend,
            terminalized: false,
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

    pub fn record(&self, evidence: OperationEvidence) -> Result<()> {
        self.backend.record(&self.operation, evidence)
    }

    pub fn succeeded(self) -> Result<()> {
        self.terminalize(AttemptTerminal::Succeeded)
    }

    pub fn failed(self, payload: Vec<u8>) -> Result<()> {
        self.terminalize(AttemptTerminal::Failed(payload))
    }

    pub fn interrupted(self) -> Result<()> {
        self.terminalize(AttemptTerminal::Interrupted)
    }

    pub fn terminalize(mut self, terminal: AttemptTerminal) -> Result<()> {
        self.terminalized = true;
        self.backend.close(&self.operation, terminal.marker())
    }
}

impl Drop for OpenAttempt<'_> {
    fn drop(&mut self) {
        if self.terminalized {
            return;
        }
        let _ = self
            .backend
            .close(&self.operation, TerminalMarker::Interrupted);
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

    #[must_use]
    pub fn status(&self) -> OperationReplayStatus<'_> {
        match self.terminal.as_ref() {
            Some(TerminalMarker::Succeeded) => OperationReplayStatus::Succeeded,
            Some(TerminalMarker::Failed(payload)) => OperationReplayStatus::Failed(payload),
            Some(TerminalMarker::Interrupted) => OperationReplayStatus::Interrupted,
            None => OperationReplayStatus::Open,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationReplayStatus<'a> {
    Open,
    Succeeded,
    Failed(&'a [u8]),
    Interrupted,
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
            owner_deadline: _,
            terminal,
        } => Ok(OperationStart::Replayed(OperationReplay::from_backend(
            operation, terminal,
        ))),
    }
}

pub fn begin_attempt<'a>(
    backend: &'a dyn OperationBackend,
    request: impl Into<AttemptRequest>,
) -> Result<AttemptStart<'a>> {
    let request = request.into();
    let operation_request = request.as_operation_request();
    match backend.start_or_replay(&operation_request)? {
        BackendOperationStart::Started => Ok(AttemptStart::Started(OpenAttempt::from_request(
            &operation_request,
            backend,
        ))),
        BackendOperationStart::Replayed {
            operation,
            owner_deadline,
            terminal,
        } => Ok(AttemptStart::Replayed(match terminal {
            Some(TerminalMarker::Succeeded) => AttemptReplay::Succeeded { operation },
            Some(TerminalMarker::Failed(payload)) => AttemptReplay::Failed { operation, payload },
            Some(TerminalMarker::Interrupted) => AttemptReplay::Interrupted { operation },
            None => AttemptReplay::Open {
                operation,
                owner_deadline,
            },
        })),
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

fn canonical_payload_digest(schema: &str, mut fields: Vec<FingerprintField>) -> Result<Vec<u8>> {
    fields.sort_by_key(|field| field.key);
    let mut seen = BTreeSet::new();
    let mut hasher = Sha256::new();
    write_component(&mut hasher, "schema");
    write_component(&mut hasher, schema);
    for field in &fields {
        if !seen.insert(field.key) {
            return Err(Error::MalformedPayload);
        }
        write_component(&mut hasher, field.key);
        match &field.value {
            FingerprintValue::String(value) => {
                write_component(&mut hasher, "string");
                write_component(&mut hasher, value);
            }
            FingerprintValue::U64(value) => {
                write_component(&mut hasher, "u64");
                hasher.update(value.to_be_bytes());
            }
            FingerprintValue::Time(value) => {
                write_component(&mut hasher, "time");
                write_time(&mut hasher, *value);
            }
        }
    }
    Ok(hasher.finalize().to_vec())
}

fn canonical_request_digest(
    actor: &PrincipalId,
    scope: &ScopeId,
    command: &CommandKind,
    payload_hash: &[u8],
    resources: &[FingerprintedResource],
    submitted_fence: Option<&SubmittedFenceFingerprint>,
    authority_epoch: GrantEpoch,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    write_component(&mut hasher, "polis.request.v1");
    write_component(&mut hasher, actor.as_str());
    write_component(&mut hasher, scope.as_str());
    write_component(&mut hasher, command.as_str());
    write_bytes(&mut hasher, payload_hash);
    for resource in resources {
        write_component(&mut hasher, resource.as_str());
    }
    match submitted_fence {
        Some(fence) => {
            write_component(&mut hasher, "submitted-fence");
            write_component(&mut hasher, fence.resource());
            write_component(&mut hasher, fence.holder());
            hasher.update(fence.epoch().to_be_bytes());
            write_bytes(&mut hasher, fence.claim_hash());
        }
        None => write_component(&mut hasher, "no-submitted-fence"),
    }
    hasher.update(authority_epoch.value().to_be_bytes());
    hasher.finalize().to_vec()
}

fn write_component(hasher: &mut Sha256, value: &str) {
    write_bytes(hasher, value.as_bytes());
}

fn write_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value);
}

fn write_time(hasher: &mut Sha256, value: SystemTime) {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            write_component(hasher, "after");
            hasher.update(duration.as_secs().to_be_bytes());
            hasher.update(duration.subsec_nanos().to_be_bytes());
        }
        Err(error) => {
            let duration = error.duration();
            write_component(hasher, "before");
            hasher.update(duration.as_secs().to_be_bytes());
            hasher.update(duration.subsec_nanos().to_be_bytes());
        }
    }
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
                    owner_deadline: existing.owner_deadline(),
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

    fn fence(epoch: u64, claim_hash: &[u8]) -> SubmittedFenceFingerprint {
        SubmittedFenceFingerprint::parse("volume:data", "node-a", epoch, claim_hash.to_vec())
            .expect("submitted fence fingerprint")
    }

    fn fingerprint(
        payload_hash: &[u8],
        authority_epoch: u64,
        submitted_fence: Option<SubmittedFenceFingerprint>,
    ) -> RequestFingerprint {
        RequestFingerprint::new(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("deploy").expect("command"),
            payload_hash.to_vec(),
            vec![
                FingerprintedResource::parse("route:app").expect("route"),
                FingerprintedResource::parse("cert:app.example.com").expect("cert"),
            ],
            submitted_fence,
            GrantEpoch::new(authority_epoch),
        )
        .expect("fingerprint")
    }

    fn request(payload_hash: &[u8], authority_epoch: u64) -> OperationRequest {
        request_with_fence(payload_hash, authority_epoch, None)
    }

    fn request_with_fence(
        payload_hash: &[u8],
        authority_epoch: u64,
        submitted_fence: Option<SubmittedFenceFingerprint>,
    ) -> OperationRequest {
        request_for(
            "op-1",
            "deploy-1",
            payload_hash,
            authority_epoch,
            submitted_fence,
        )
    }

    fn request_for(
        operation: &str,
        idempotency: &str,
        payload_hash: &[u8],
        authority_epoch: u64,
        submitted_fence: Option<SubmittedFenceFingerprint>,
    ) -> OperationRequest {
        OperationRequest::new(
            OperationId::parse(operation).expect("operation id"),
            IdempotencyKey::parse(idempotency).expect("idempotency key"),
            fingerprint(payload_hash, authority_epoch, submitted_fence),
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
    fn replay_status_makes_open_and_terminal_states_explicit() {
        let operation = OperationId::parse("op-1").expect("operation");

        assert_eq!(
            OperationReplay::from_backend(operation.clone(), None).status(),
            OperationReplayStatus::Open
        );
        assert_eq!(
            OperationReplay::from_backend(operation.clone(), Some(TerminalMarker::Succeeded))
                .status(),
            OperationReplayStatus::Succeeded
        );
        assert_eq!(
            OperationReplay::from_backend(
                operation.clone(),
                Some(TerminalMarker::Failed(b"failed".to_vec())),
            )
            .status(),
            OperationReplayStatus::Failed(b"failed")
        );
        assert_eq!(
            OperationReplay::from_backend(operation, Some(TerminalMarker::Interrupted)).status(),
            OperationReplayStatus::Interrupted
        );
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
    fn same_payload_with_different_submitted_fence_conflicts() {
        let store = MemoryOperationStore::new();
        let _started = start_or_replay(
            &store,
            request_with_fence(&[1, 2, 3], 7, Some(fence(3, b"claim-hash-a"))),
        )
        .expect("started");

        assert_eq!(
            start_or_replay(
                &store,
                request_with_fence(&[1, 2, 3], 7, Some(fence(4, b"claim-hash-b"))),
            ),
            Err(Error::Conflict)
        );
    }

    #[test]
    fn same_payload_with_added_submitted_fence_conflicts() {
        let store = MemoryOperationStore::new();
        let _started = start_or_replay(&store, request(&[1, 2, 3], 7)).expect("started");

        assert_eq!(
            start_or_replay(
                &store,
                request_with_fence(&[1, 2, 3], 7, Some(fence(3, b"claim-hash-a"))),
            ),
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

    #[test]
    fn attempt_success_consumes_and_terminalizes_once() {
        let store = MemoryOperationStore::new();
        let AttemptStart::Started(attempt) =
            begin_attempt(&store, request(&[1, 2, 3], 7)).expect("started")
        else {
            panic!("expected started attempt");
        };

        attempt.succeeded().expect("succeeded");

        assert_eq!(
            store
                .terminal
                .borrow()
                .get(&OperationId::parse("op-1").expect("operation"))
                .cloned(),
            Some(TerminalMarker::Succeeded)
        );
    }

    #[test]
    fn dropping_open_attempt_marks_interrupted() {
        let store = MemoryOperationStore::new();
        {
            let AttemptStart::Started(_attempt) =
                begin_attempt(&store, request(&[1, 2, 3], 7)).expect("started")
            else {
                panic!("expected started attempt");
            };
        }

        assert_eq!(
            store
                .terminal
                .borrow()
                .get(&OperationId::parse("op-1").expect("operation"))
                .cloned(),
            Some(TerminalMarker::Interrupted)
        );
    }

    #[test]
    fn explicit_attempt_terminalization_failure_is_returned() {
        let store = MemoryOperationStore::new();
        let request = request_for(
            "op-terminal-fails",
            "idem-terminal-fails",
            &[1, 2, 3],
            7,
            None,
        );
        let AttemptStart::Started(attempt) = begin_attempt(&store, request).expect("started")
        else {
            panic!("expected started attempt");
        };
        store.terminal.borrow_mut().insert(
            OperationId::parse("op-terminal-fails").expect("operation"),
            TerminalMarker::Interrupted,
        );

        assert_eq!(attempt.succeeded(), Err(Error::TerminalAlreadyWritten));
    }

    #[test]
    fn fingerprint_builder_uses_canonical_digest_for_fields_resources_and_fence() {
        let first = RequestFingerprint::builder(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("deploy").expect("command"),
            "ployz.deploy.https.v1",
            GrantEpoch::new(7),
        )
        .field("hostname", "app.example.com")
        .field_u64("generation", 11)
        .field_time("deadline", UNIX_EPOCH + Duration::from_secs(10))
        .resource(FingerprintedResource::parse("domain:app.example.com").expect("resource"))
        .submitted_fence(fence(3, b"claim-hash-a"))
        .finish()
        .expect("fingerprint");

        let second = RequestFingerprint::builder(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("deploy").expect("command"),
            "ployz.deploy.https.v1",
            GrantEpoch::new(7),
        )
        .field("hostname", "app.example.com")
        .field_u64("generation", 12)
        .field_time("deadline", UNIX_EPOCH + Duration::from_secs(10))
        .resource(FingerprintedResource::parse("domain:app.example.com").expect("resource"))
        .submitted_fence(fence(3, b"claim-hash-a"))
        .finish()
        .expect("fingerprint");

        assert_ne!(first.payload_hash(), second.payload_hash());
    }

    #[test]
    fn fingerprint_builder_is_stable_across_field_order() {
        let first = RequestFingerprint::builder(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("deploy").expect("command"),
            "ployz.deploy.https.v1",
            GrantEpoch::new(7),
        )
        .field("hostname", "app.example.com")
        .field_u64("generation", 11)
        .resource(FingerprintedResource::parse("domain:app.example.com").expect("resource"))
        .finish()
        .expect("fingerprint");

        let second = RequestFingerprint::builder(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("deploy").expect("command"),
            "ployz.deploy.https.v1",
            GrantEpoch::new(7),
        )
        .field_u64("generation", 11)
        .field("hostname", "app.example.com")
        .resource(FingerprintedResource::parse("domain:app.example.com").expect("resource"))
        .finish()
        .expect("fingerprint");

        assert_eq!(first, second);
    }

    #[test]
    fn fingerprint_builder_rejects_duplicate_fields() {
        let result = RequestFingerprint::builder(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
            CommandKind::parse("deploy").expect("command"),
            "ployz.deploy.https.v1",
            GrantEpoch::new(7),
        )
        .field("hostname", "app.example.com")
        .field("hostname", "other.example.com")
        .resource(FingerprintedResource::parse("domain:app.example.com").expect("resource"))
        .finish();

        assert_eq!(result, Err(Error::MalformedPayload));
    }
}
