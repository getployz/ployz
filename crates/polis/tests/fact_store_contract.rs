use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use polis::{
    Authority, AuthorityContext, AuthorityDecision, AuthorityService, CandidateStatus,
    FactAppendFingerprint, FactAppendOutcome, FactAppendRequest, FactAppendScope, FactCandidate,
    FactCandidateSet, FactConflict, FactCursor, FactGrantAuthority, FactGrantDecision,
    FactGrantPurpose, FactGrantService, FactKey, FactKind, FactPayload, FactPayloadBatch,
    FactPayloadReadFailure, FactQuery, FactReceipt, FactReplayKey, FactStore, FactTarget,
    GrantEpoch, IdempotencyKey, OperationId, PrincipalId, ResourceId, ScopeId,
};

#[derive(Default)]
struct ContractFactStore {
    next_cursor: Cell<u64>,
    by_replay: RefCell<BTreeMap<FactReplayKey, StoredAppend>>,
    records: RefCell<Vec<(FactReceipt, FactPayload)>>,
}

impl FactStore for ContractFactStore {
    fn append(&self, request: FactAppendRequest) -> polis::Result<FactAppendOutcome> {
        let append = match request.validate() {
            Ok(append) => append,
            Err(rejection) => return Ok(FactAppendOutcome::Rejected(rejection)),
        };

        if let Some(existing) = self.by_replay.borrow().get(append.replay_key()) {
            if existing.fingerprint == *append.fingerprint() {
                return Ok(FactAppendOutcome::Replayed(Box::new(
                    existing.receipt.clone(),
                )));
            }
            return Ok(FactAppendOutcome::Conflict(Box::new(
                FactConflict::IdempotencyKeyReuse {
                    existing: Box::new(existing.receipt.clone()),
                },
            )));
        }

        let cursor = self.next_cursor.get() + 1;
        self.next_cursor.set(cursor);
        let receipt = FactReceipt::from_validated_append(&append, FactCursor::new(cursor));
        let replay_key = append.replay_key().clone();
        let fingerprint = append.fingerprint().clone();
        let payload = append.request().payload().clone();

        self.records
            .borrow_mut()
            .push((receipt.clone(), payload.clone()));
        self.by_replay.borrow_mut().insert(
            replay_key,
            StoredAppend {
                fingerprint,
                receipt: receipt.clone(),
            },
        );

        Ok(FactAppendOutcome::Appended(Box::new(receipt)))
    }

    fn list_candidates(&self, query: FactQuery) -> polis::Result<FactCandidateSet> {
        let candidates = self
            .records
            .borrow()
            .iter()
            .filter(|(receipt, _payload)| query.matches(receipt))
            .map(|(receipt, _payload)| {
                FactCandidate::new(receipt.clone(), CandidateStatus::Verified)
            })
            .collect();

        Ok(FactCandidateSet::complete_prefix(
            candidates,
            FactCursor::new(self.next_cursor.get()),
        ))
    }

    fn read_payloads(&self, candidates: &[FactCandidate]) -> polis::Result<FactPayloadBatch> {
        let records = self.records.borrow();
        let mut payloads = BTreeMap::new();
        let mut failures = BTreeMap::new();

        for candidate in candidates {
            let Some((receipt, payload)) = records
                .iter()
                .find(|(receipt, _payload)| receipt.id() == candidate.receipt().id())
            else {
                failures.insert(
                    candidate.receipt().id().clone(),
                    FactPayloadReadFailure::UnknownCandidate,
                );
                continue;
            };

            if receipt != candidate.receipt() {
                failures.insert(
                    candidate.receipt().id().clone(),
                    FactPayloadReadFailure::CandidateMismatch,
                );
                continue;
            }

            if payload.digest() != *candidate.receipt().payload_digest() {
                failures.insert(
                    candidate.receipt().id().clone(),
                    FactPayloadReadFailure::DigestMismatch,
                );
                continue;
            }

            payloads.insert(candidate.receipt().id().clone(), payload.clone());
        }

        Ok(FactPayloadBatch::from_parts(payloads, failures))
    }
}

#[derive(Clone)]
struct StoredAppend {
    fingerprint: FactAppendFingerprint,
    receipt: FactReceipt,
}

struct AllowAuthority;

impl Authority for AllowAuthority {
    fn decide(&self, _principal: &PrincipalId, _scope: &ScopeId) -> AuthorityDecision {
        AuthorityDecision::allowed(GrantEpoch::new(7))
    }
}

struct AllowGrantAuthority;

impl FactGrantAuthority for AllowGrantAuthority {
    fn decide(
        &self,
        _authority: &AuthorityContext,
        _target: &FactTarget,
        _purpose: FactGrantPurpose,
    ) -> FactGrantDecision {
        FactGrantDecision::allowed()
    }
}

#[test]
fn non_memory_fact_store_can_use_public_backend_contract() {
    let store = ContractFactStore::default();
    let authority = AuthorityService::new(AllowAuthority)
        .authorize::<FactAppendScope>(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
        )
        .expect("authorized");
    let target = FactTarget::new(
        ResourceId::parse("machine:node-a").expect("resource"),
        FactKey::parse("membership/node-a").expect("key"),
        FactKind::parse("ployz.machine.joined.v1").expect("kind"),
    );
    let grant = FactGrantService::new(AllowGrantAuthority)
        .issue_append(&authority, target.clone())
        .expect("write grant");
    let request = FactAppendRequest::new(
        OperationId::parse("op-1").expect("operation"),
        IdempotencyKey::parse("idem-1").expect("idempotency"),
        authority,
        grant,
        target,
        FactPayload::new(b"joined".to_vec()).expect("payload"),
        None,
    );

    let appended = store.append(request).expect("append");
    let FactAppendOutcome::Appended(receipt) = appended else {
        panic!("expected append");
    };
    let candidates = store
        .list_candidates(FactQuery::new(ScopeId::parse("cluster").expect("scope")))
        .expect("candidates");
    let payloads = store
        .read_payloads(candidates.as_slice())
        .expect("payloads");

    assert_eq!(candidates.source_cursor(), receipt.cursor());
    assert_eq!(candidates.len(), 1);
    assert!(payloads.is_complete());
    assert_eq!(
        payloads
            .get(candidates.iter().next().expect("candidate"))
            .map(FactPayload::as_bytes),
        Some(b"joined".as_slice())
    );
}

#[test]
fn non_memory_fact_store_reuses_public_request_validation() {
    let store = ContractFactStore::default();
    let authority = AuthorityService::new(AllowAuthority)
        .authorize::<FactAppendScope>(
            PrincipalId::parse("node-a").expect("principal"),
            ScopeId::parse("cluster").expect("scope"),
        )
        .expect("authorized");
    let allowed_target = FactTarget::new(
        ResourceId::parse("machine:node-a").expect("resource"),
        FactKey::parse("membership/node-a").expect("key"),
        FactKind::parse("ployz.machine.joined.v1").expect("kind"),
    );
    let request_target = FactTarget::new(
        ResourceId::parse("machine:node-a").expect("resource"),
        FactKey::parse("membership/other").expect("key"),
        FactKind::parse("ployz.machine.joined.v1").expect("kind"),
    );
    let grant = FactGrantService::new(AllowGrantAuthority)
        .issue_append(&authority, allowed_target)
        .expect("write grant");
    let request = FactAppendRequest::new(
        OperationId::parse("op-1").expect("operation"),
        IdempotencyKey::parse("idem-1").expect("idempotency"),
        authority,
        grant,
        request_target,
        FactPayload::new(b"joined".to_vec()).expect("payload"),
        None,
    );

    let outcome = store.append(request).expect("append");

    assert_eq!(
        outcome,
        FactAppendOutcome::Rejected(polis::FactRejection::Unauthorized)
    );
}
