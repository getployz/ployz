//! Bounded request/reply and mutation receipt primitives.

use std::time::SystemTime;

use crate::authority::Authorized;
use crate::claims::ClaimGuard;
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
pub struct CallIntent {
    sender: PrincipalId,
    target: TargetId,
    operation: OperationId,
    idempotency: IdempotencyKey,
    payload_hash: PayloadHash,
    deadline: SystemTime,
}

impl CallIntent {
    #[must_use]
    pub fn new(
        sender: PrincipalId,
        target: TargetId,
        operation: OperationId,
        idempotency: IdempotencyKey,
        payload_hash: PayloadHash,
        deadline: SystemTime,
    ) -> Self {
        Self {
            sender,
            target,
            operation,
            idempotency,
            payload_hash,
            deadline,
        }
    }

    #[must_use]
    pub fn sender(&self) -> &PrincipalId {
        &self.sender
    }

    #[must_use]
    pub fn target(&self) -> &TargetId {
        &self.target
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
    pub fn payload_hash(&self) -> &PayloadHash {
        &self.payload_hash
    }

    #[must_use]
    pub fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEnvelope {
    intent: CallIntent,
    authority: Authorized,
    fence: Option<ClaimGuard>,
}

impl CallEnvelope {
    pub fn try_new(
        intent: CallIntent,
        authority: Authorized,
        fence: Option<ClaimGuard>,
    ) -> Result<Self> {
        if intent.sender() != authority.context().principal() {
            return Err(Error::Unauthorized);
        }
        Ok(Self {
            intent,
            authority,
            fence,
        })
    }

    #[must_use]
    pub fn sender(&self) -> &PrincipalId {
        self.intent.sender()
    }

    #[must_use]
    pub fn target(&self) -> &TargetId {
        self.intent.target()
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        self.intent.operation()
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        self.intent.idempotency()
    }

    #[must_use]
    pub fn payload_hash(&self) -> &PayloadHash {
        self.intent.payload_hash()
    }

    #[must_use]
    pub fn authority(&self) -> &Authorized {
        &self.authority
    }

    #[must_use]
    pub fn fence(&self) -> Option<&ClaimGuard> {
        self.fence.as_ref()
    }

    #[must_use]
    pub fn deadline(&self) -> SystemTime {
        self.intent.deadline()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallReply<T = Vec<u8>, E = Error> {
    Success(T),
    Failure(E),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationReceipt<T = Vec<u8>, E = Error> {
    envelope: CallEnvelope,
    reply: CallReply<T, E>,
    recorded_at: SystemTime,
}

impl<T, E> MutationReceipt<T, E> {
    #[must_use]
    pub fn new(envelope: CallEnvelope, reply: CallReply<T, E>, recorded_at: SystemTime) -> Self {
        Self {
            envelope,
            reply,
            recorded_at,
        }
    }

    #[must_use]
    pub fn envelope(&self) -> &CallEnvelope {
        &self.envelope
    }

    #[must_use]
    pub fn reply(&self) -> &CallReply<T, E> {
        &self.reply
    }

    #[must_use]
    pub fn recorded_at(&self) -> SystemTime {
        self.recorded_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptWrite<T = Vec<u8>, E = Error> {
    Recorded(MutationReceipt<T, E>),
    Replayed(MutationReceipt<T, E>),
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
                target: envelope.target().clone(),
                operation: envelope.operation().clone(),
                idempotency: envelope.idempotency().clone(),
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
            if existing.envelope().payload_hash() != receipt.envelope().payload_hash() {
                return Err(Error::Conflict);
            }
            Ok(ReceiptWrite::Replayed(existing.clone()))
        }
    }

    fn envelope(payload_hash: &[u8]) -> CallEnvelope {
        let principal = PrincipalId::parse("node-a").expect("principal");
        let scope = ScopeId::parse("cluster").expect("scope");
        let authority = Authorized::new(AuthorityContext::new(
            principal.clone(),
            scope,
            GrantEpoch::new(1),
        ));
        let intent = CallIntent::new(
            principal,
            TargetId::parse("node-b").expect("target"),
            OperationId::parse("op-1").expect("operation"),
            IdempotencyKey::parse("call-1").expect("idempotency"),
            PayloadHash::new(payload_hash.to_vec()).expect("payload hash"),
            UNIX_EPOCH + Duration::from_secs(10),
        );
        CallEnvelope::try_new(intent, authority, None).expect("envelope")
    }

    #[test]
    fn call_envelope_rejects_sender_authority_mismatch() {
        let sender = PrincipalId::parse("node-a").expect("sender");
        let authority_principal = PrincipalId::parse("node-b").expect("authority principal");
        let authority = Authorized::new(AuthorityContext::new(
            authority_principal,
            ScopeId::parse("cluster").expect("scope"),
            GrantEpoch::new(1),
        ));
        let intent = CallIntent::new(
            sender,
            TargetId::parse("node-c").expect("target"),
            OperationId::parse("op-1").expect("operation"),
            IdempotencyKey::parse("call-1").expect("idempotency"),
            PayloadHash::new(vec![1]).expect("payload hash"),
            UNIX_EPOCH + Duration::from_secs(10),
        );

        assert_eq!(
            CallEnvelope::try_new(intent, authority, None),
            Err(Error::Unauthorized)
        );
    }

    fn receipt(payload_hash: &[u8], reply: CallReply) -> MutationReceipt {
        MutationReceipt::new(
            envelope(payload_hash),
            reply,
            UNIX_EPOCH + Duration::from_secs(1),
        )
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
        assert_eq!(receipt.reply(), &CallReply::Failure(Error::StaleFence));
    }

    #[test]
    fn typed_mutation_receipt_preserves_failure_type() {
        #[derive(Debug, Clone, PartialEq, Eq)]
        enum DomainReply {
            Ready,
        }
        #[derive(Debug, Clone, PartialEq, Eq)]
        enum DomainFailure {
            CertificateUnusable,
        }

        let receipt = MutationReceipt::new(
            envelope(&[1]),
            CallReply::<DomainReply, DomainFailure>::Failure(DomainFailure::CertificateUnusable),
            UNIX_EPOCH + Duration::from_secs(1),
        );

        assert!(matches!(
            receipt.reply(),
            CallReply::Failure(DomainFailure::CertificateUnusable)
        ));

        let success = CallReply::<DomainReply, DomainFailure>::Success(DomainReply::Ready);
        assert!(matches!(success, CallReply::Success(DomainReply::Ready)));
    }
}
