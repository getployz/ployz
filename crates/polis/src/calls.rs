//! Bounded request/reply and mutation receipt primitives.

use std::time::SystemTime;

use crate::authority::AuthorityContext;
use crate::claims::FenceToken;
use crate::identity::PrincipalId;
use crate::operations::{IdempotencyKey, OperationId};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetId(String);

impl TargetId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PayloadHash(Vec<u8>);

impl PayloadHash {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEnvelope {
    pub sender: PrincipalId,
    pub target: TargetId,
    pub operation: OperationId,
    pub idempotency: IdempotencyKey,
    pub payload_hash: PayloadHash,
    pub authority: AuthorityContext,
    pub fence: Option<FenceToken>,
    pub deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallReply {
    Success(Vec<u8>),
    Failure(Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationReceipt {
    pub envelope: CallEnvelope,
    pub reply: CallReply,
    pub recorded_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptWrite {
    Recorded(MutationReceipt),
    Replayed(MutationReceipt),
}

pub trait MutationReceiptStore {
    fn record_or_replay(&mut self, receipt: MutationReceipt) -> Result<ReceiptWrite>;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::authority::{AuthorityContext, GrantEpoch};
    use crate::identity::{PrincipalId, ScopeId};

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct ReceiptLookupKey {
        target: TargetId,
        operation: OperationId,
        idempotency: IdempotencyKey,
    }

    impl ReceiptLookupKey {
        fn from_envelope(envelope: &CallEnvelope) -> Self {
            Self {
                target: envelope.target.clone(),
                operation: envelope.operation.clone(),
                idempotency: envelope.idempotency.clone(),
            }
        }
    }

    struct MemoryReceiptStore {
        receipts: BTreeMap<ReceiptLookupKey, MutationReceipt>,
    }

    impl MemoryReceiptStore {
        fn new() -> Self {
            Self {
                receipts: BTreeMap::new(),
            }
        }
    }

    impl MutationReceiptStore for MemoryReceiptStore {
        fn record_or_replay(&mut self, receipt: MutationReceipt) -> Result<ReceiptWrite> {
            let key = ReceiptLookupKey::from_envelope(&receipt.envelope);
            let Some(existing) = self.receipts.get(&key) else {
                self.receipts.insert(key, receipt.clone());
                return Ok(ReceiptWrite::Recorded(receipt));
            };
            if existing.envelope.payload_hash != receipt.envelope.payload_hash {
                return Err(Error::Conflict);
            }
            Ok(ReceiptWrite::Replayed(existing.clone()))
        }
    }

    fn envelope(payload_hash: &[u8]) -> CallEnvelope {
        let principal = PrincipalId::parse("node-a").expect("principal");
        let scope = ScopeId::parse("cluster").expect("scope");
        CallEnvelope {
            sender: principal.clone(),
            target: TargetId::parse("node-b").expect("target"),
            operation: OperationId::parse("op-1").expect("operation"),
            idempotency: IdempotencyKey::parse("call-1").expect("idempotency"),
            payload_hash: PayloadHash::new(payload_hash.to_vec()).expect("payload hash"),
            authority: AuthorityContext::new(principal, scope, GrantEpoch::new(1)),
            fence: None,
            deadline: UNIX_EPOCH + Duration::from_secs(10),
        }
    }

    fn receipt(payload_hash: &[u8], reply: CallReply) -> MutationReceipt {
        MutationReceipt {
            envelope: envelope(payload_hash),
            reply,
            recorded_at: UNIX_EPOCH + Duration::from_secs(1),
        }
    }

    #[test]
    fn lost_reply_retry_replays_recorded_receipt() {
        let mut store = MemoryReceiptStore::new();
        let first = receipt(&[1], CallReply::Success(vec![7]));
        let second = receipt(&[1], CallReply::Success(vec![9]));

        let _recorded = store.record_or_replay(first).expect("recorded");
        let replayed = store.record_or_replay(second).expect("replayed");

        assert!(matches!(replayed, ReceiptWrite::Replayed(_)));
    }

    #[test]
    fn same_idempotency_with_different_payload_conflicts() {
        let mut store = MemoryReceiptStore::new();
        let _recorded = store
            .record_or_replay(receipt(&[1], CallReply::Success(vec![7])))
            .expect("recorded");

        assert_eq!(
            store.record_or_replay(receipt(&[2], CallReply::Success(vec![7]))),
            Err(Error::Conflict)
        );
    }

    #[test]
    fn failure_receipts_are_durable_results_too() {
        let mut store = MemoryReceiptStore::new();
        let first = receipt(&[1], CallReply::Failure(Error::StaleFence));
        let second = receipt(&[1], CallReply::Success(vec![9]));

        let _recorded = store.record_or_replay(first).expect("recorded");
        let replayed = store.record_or_replay(second).expect("replayed");

        let ReceiptWrite::Replayed(receipt) = replayed else {
            panic!("expected replayed receipt");
        };
        assert_eq!(receipt.reply, CallReply::Failure(Error::StaleFence));
    }
}
