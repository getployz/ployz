//! Operation evidence and idempotency primitives.

use std::time::SystemTime;

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
    bytes: Vec<u8>,
}

impl RequestFingerprint {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self { bytes })
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

    fn request(fingerprint: &[u8]) -> OperationRequest {
        OperationRequest {
            operation: OperationId::parse("op-1").expect("operation id"),
            idempotency: IdempotencyKey::parse("deploy-1").expect("idempotency key"),
            fingerprint: RequestFingerprint::new(fingerprint.to_vec()).expect("fingerprint"),
            owner_deadline: UNIX_EPOCH + Duration::from_secs(10),
        }
    }

    #[test]
    fn same_idempotency_and_fingerprint_replays_record() {
        let mut store = MemoryOperationStore::new();
        let _started = store.start_or_replay(request(&[1, 2, 3])).expect("started");

        let replayed = store
            .start_or_replay(request(&[1, 2, 3]))
            .expect("replayed");

        assert!(matches!(replayed, OperationStart::Replayed(_)));
    }

    #[test]
    fn same_idempotency_with_different_fingerprint_conflicts() {
        let mut store = MemoryOperationStore::new();
        let _started = store.start_or_replay(request(&[1, 2, 3])).expect("started");

        assert_eq!(store.start_or_replay(request(&[9])), Err(Error::Conflict));
    }

    #[test]
    fn second_terminal_marker_is_rejected() {
        let mut record = OperationRecord::start(request(&[1]));
        record
            .terminalize(TerminalMarker::Succeeded)
            .expect("terminal marker");

        assert_eq!(
            record.terminalize(TerminalMarker::Interrupted),
            Err(Error::TerminalAlreadyWritten)
        );
    }
}
