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
    pub actor: PrincipalId,
    pub scope: ScopeId,
    pub command: CommandKind,
    pub payload_hash: Vec<u8>,
    pub resources: Vec<FingerprintedResource>,
    pub authority_epoch: GrantEpoch,
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
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub fingerprint: RequestFingerprint,
    pub owner_deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStart {
    Started(OperationRecord),
    Replayed(OperationRecord),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub fingerprint: RequestFingerprint,
    pub owner_deadline: SystemTime,
    pub evidence: Vec<OperationEvidence>,
    pub terminal: Option<TerminalMarker>,
}

impl OperationRecord {
    #[must_use]
    pub fn start(request: OperationRequest) -> Self {
        Self {
            operation: request.operation,
            idempotency: request.idempotency,
            fingerprint: request.fingerprint,
            owner_deadline: request.owner_deadline,
            evidence: Vec::new(),
            terminal: None,
        }
    }

    pub fn append_evidence(&mut self, evidence: OperationEvidence) -> Result<()> {
        if self.terminal.is_some() {
            return Err(Error::TerminalAlreadyWritten);
        }
        self.evidence.push(evidence);
        Ok(())
    }

    pub fn terminalize(&mut self, marker: TerminalMarker) -> Result<()> {
        if self.terminal.is_some() {
            return Err(Error::TerminalAlreadyWritten);
        }
        self.terminal = Some(marker);
        Ok(())
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
    CleanupObligation(CleanupObligation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupObligation {
    pub artifact_id: String,
    pub owner: CleanupOwner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOwner {
    CommandResume,
    OperatorCommand,
    SupervisedRole(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMarker {
    Succeeded,
    Failed(Vec<u8>),
    Interrupted,
}

pub trait OperationStore {
    fn start_or_replay(&mut self, request: OperationRequest) -> Result<OperationStart>;

    fn append_evidence(
        &mut self,
        operation: &OperationId,
        evidence: OperationEvidence,
    ) -> Result<()>;

    fn terminalize(&mut self, operation: &OperationId, marker: TerminalMarker) -> Result<()>;
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
    use std::collections::BTreeMap;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    struct MemoryOperationStore {
        by_idempotency: BTreeMap<IdempotencyKey, OperationRecord>,
    }

    impl MemoryOperationStore {
        fn new() -> Self {
            Self {
                by_idempotency: BTreeMap::new(),
            }
        }
    }

    impl OperationStore for MemoryOperationStore {
        fn start_or_replay(&mut self, request: OperationRequest) -> Result<OperationStart> {
            if let Some(existing) = self.by_idempotency.get(&request.idempotency) {
                if existing.fingerprint != request.fingerprint {
                    return Err(Error::Conflict);
                }
                return Ok(OperationStart::Replayed(existing.clone()));
            }
            let record = OperationRecord::start(request);
            self.by_idempotency
                .insert(record.idempotency.clone(), record.clone());
            Ok(OperationStart::Started(record))
        }

        fn append_evidence(
            &mut self,
            operation: &OperationId,
            evidence: OperationEvidence,
        ) -> Result<()> {
            let Some(record) = self
                .by_idempotency
                .values_mut()
                .find(|record| &record.operation == operation)
            else {
                return Err(Error::Conflict);
            };
            record.append_evidence(evidence)
        }

        fn terminalize(&mut self, operation: &OperationId, marker: TerminalMarker) -> Result<()> {
            let Some(record) = self
                .by_idempotency
                .values_mut()
                .find(|record| &record.operation == operation)
            else {
                return Err(Error::Conflict);
            };
            record.terminalize(marker)
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
        OperationRequest {
            operation: OperationId::parse("op-1").expect("operation id"),
            idempotency: IdempotencyKey::parse("deploy-1").expect("idempotency key"),
            fingerprint: fingerprint(payload_hash, authority_epoch),
            owner_deadline: UNIX_EPOCH + Duration::from_secs(10),
        }
    }

    #[test]
    fn same_idempotency_and_fingerprint_replays_record() {
        let mut store = MemoryOperationStore::new();
        let _started = store
            .start_or_replay(request(&[1, 2, 3], 7))
            .expect("started");

        let replayed = store
            .start_or_replay(request(&[1, 2, 3], 7))
            .expect("replayed");

        assert!(matches!(replayed, OperationStart::Replayed(_)));
    }

    #[test]
    fn same_idempotency_with_different_fingerprint_conflicts() {
        let mut store = MemoryOperationStore::new();
        let _started = store
            .start_or_replay(request(&[1, 2, 3], 7))
            .expect("started");

        assert_eq!(
            store.start_or_replay(request(&[9], 7)),
            Err(Error::Conflict)
        );
    }

    #[test]
    fn same_payload_with_different_authority_epoch_conflicts() {
        let mut store = MemoryOperationStore::new();
        let _started = store
            .start_or_replay(request(&[1, 2, 3], 7))
            .expect("started");

        assert_eq!(
            store.start_or_replay(request(&[1, 2, 3], 8)),
            Err(Error::Conflict)
        );
    }

    #[test]
    fn second_terminal_marker_is_rejected() {
        let mut record = OperationRecord::start(request(&[1], 7));
        record
            .terminalize(TerminalMarker::Succeeded)
            .expect("terminal marker");

        assert_eq!(
            record.terminalize(TerminalMarker::Interrupted),
            Err(Error::TerminalAlreadyWritten)
        );
    }
}
