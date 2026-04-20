use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_api::{
    CoordinationAbortRequest, CoordinationCommitPayload, CoordinationCommitRequest,
    CoordinationLockKey, CoordinationOperation, CoordinationPreparePayload,
    CoordinationPrepareRequest, CoordinationRenewPayload, CoordinationRenewRequest, DaemonPayload,
    DaemonResponse,
};

use crate::daemon::DaemonState;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedRecord {
    owner_id: String,
    nonce: String,
    operation: CoordinationOperation,
    expires_at: u64,
    prepare_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommittedRecord {
    owner_id: String,
    nonce: String,
    operation: CoordinationOperation,
    committed_at: u64,
}

#[derive(Debug, Default)]
pub(crate) struct CoordinationLedger {
    prepared_by_key: BTreeMap<String, PreparedRecord>,
    prepared_by_token: BTreeMap<String, String>,
    committed_by_key: BTreeMap<String, CommittedRecord>,
}

impl CoordinationLedger {
    fn prepare(
        &mut self,
        request: CoordinationPrepareRequest,
        now: u64,
    ) -> CoordinationPreparePayload {
        let CoordinationPrepareRequest {
            owner_id,
            nonce,
            lease_ttl_secs,
            operation,
        } = request;
        if lease_ttl_secs == 0 {
            return CoordinationPreparePayload {
                accepted: false,
                reason: Some("lease_ttl_secs must be greater than zero".into()),
                prepare_token: None,
            };
        }
        if nonce.is_empty() {
            return CoordinationPreparePayload {
                accepted: false,
                reason: Some("nonce must not be empty".into()),
                prepare_token: None,
            };
        }
        if owner_id.is_empty() {
            return CoordinationPreparePayload {
                accepted: false,
                reason: Some("owner_id must not be empty".into()),
                prepare_token: None,
            };
        }

        let key = operation_key(&operation);
        self.prune_expired_for_key(&key, now);

        if let Some(committed) = self.committed_by_key.get(&key) {
            if committed.owner_id == owner_id
                && committed.nonce == nonce
                && committed.operation == operation
            {
                return CoordinationPreparePayload {
                    accepted: true,
                    reason: None,
                    prepare_token: Some(format!("committed:{key}:{nonce}")),
                };
            }
            return CoordinationPreparePayload {
                accepted: false,
                reason: Some("key already committed by a different operation".into()),
                prepare_token: None,
            };
        }

        if let Some(existing) = self.prepared_by_key.get_mut(&key) {
            if existing.owner_id == owner_id
                && existing.nonce == nonce
                && existing.operation == operation
            {
                existing.expires_at = now.saturating_add(lease_ttl_secs);
                return CoordinationPreparePayload {
                    accepted: true,
                    reason: None,
                    prepare_token: Some(existing.prepare_token.clone()),
                };
            }
            return CoordinationPreparePayload {
                accepted: false,
                reason: Some("key is already prepared by another operation".into()),
                prepare_token: None,
            };
        }

        let prepare_token = format!("prep:{key}:{nonce}");
        let record = PreparedRecord {
            owner_id,
            nonce,
            operation,
            expires_at: now.saturating_add(lease_ttl_secs),
            prepare_token: prepare_token.clone(),
        };
        self.prepared_by_token
            .insert(prepare_token.clone(), key.clone());
        self.prepared_by_key.insert(key, record);
        CoordinationPreparePayload {
            accepted: true,
            reason: None,
            prepare_token: Some(prepare_token),
        }
    }

    fn commit(
        &mut self,
        request: CoordinationCommitRequest,
        now: u64,
    ) -> (CoordinationCommitPayload, Option<CoordinationOperation>) {
        let CoordinationCommitRequest {
            owner_id,
            nonce,
            prepare_tokens,
            operation,
        } = request;
        if nonce.is_empty() {
            return (
                CoordinationCommitPayload {
                    committed: false,
                    reason: Some("nonce must not be empty".into()),
                },
                None,
            );
        }
        if owner_id.is_empty() {
            return (
                CoordinationCommitPayload {
                    committed: false,
                    reason: Some("owner_id must not be empty".into()),
                },
                None,
            );
        }

        let key = operation_key(&operation);
        self.prune_expired_for_key(&key, now);

        if let Some(existing) = self.committed_by_key.get(&key) {
            if existing.owner_id == owner_id && existing.nonce == nonce {
                return (
                    CoordinationCommitPayload {
                        committed: true,
                        reason: None,
                    },
                    None,
                );
            }
            return (
                CoordinationCommitPayload {
                    committed: false,
                    reason: Some("key already committed by a different operation".into()),
                },
                None,
            );
        }

        let Some(prepared) = self.prepared_by_key.get(&key) else {
            return (
                CoordinationCommitPayload {
                    committed: false,
                    reason: Some("missing prepare for key".into()),
                },
                None,
            );
        };

        if prepared.owner_id != owner_id
            || prepared.nonce != nonce
            || !commit_matches_prepared_operation(&prepared.operation, &operation)
        {
            return (
                CoordinationCommitPayload {
                    committed: false,
                    reason: Some("prepare does not match commit request".into()),
                },
                None,
            );
        }

        if prepare_tokens.is_empty() {
            return (
                CoordinationCommitPayload {
                    committed: false,
                    reason: Some("prepare_tokens must include the key prepare token".into()),
                },
                None,
            );
        }

        if !prepare_tokens
            .iter()
            .any(|token| token == &prepared.prepare_token)
        {
            return (
                CoordinationCommitPayload {
                    committed: false,
                    reason: Some("prepare token did not match key prepare".into()),
                },
                None,
            );
        }

        let prepared_operation = prepared.operation.clone();
        let record = CommittedRecord {
            owner_id,
            nonce,
            operation,
            committed_at: now,
        };

        if let Some(prepared_key) = self.prepared_by_token.remove(&prepared.prepare_token)
            && prepared_key == key
        {
            self.prepared_by_key.remove(&prepared_key);
        }
        self.committed_by_key.insert(key, record);

        (
            CoordinationCommitPayload {
                committed: true,
                reason: None,
            },
            Some(prepared_operation),
        )
    }

    fn abort(&mut self, request: CoordinationAbortRequest, now: u64) {
        let CoordinationAbortRequest {
            owner_id,
            nonce,
            operation,
        } = request;
        if nonce.is_empty() {
            return;
        }
        if owner_id.is_empty() {
            return;
        }

        let key = operation_key(&operation);
        self.prune_expired_for_key(&key, now);

        let Some(prepared) = self.prepared_by_key.get(&key) else {
            return;
        };
        if prepared.owner_id != owner_id || prepared.nonce != nonce {
            return;
        }

        let prepare_token = prepared.prepare_token.clone();
        self.prepared_by_key.remove(&key);
        self.prepared_by_token.remove(&prepare_token);
    }

    fn prune_expired_for_key(&mut self, key: &str, now: u64) {
        let Some(prepared) = self.prepared_by_key.get(key) else {
            return;
        };

        if prepared.expires_at > now {
            return;
        }

        let prepare_token = prepared.prepare_token.clone();
        self.prepared_by_key.remove(key);
        self.prepared_by_token.remove(&prepare_token);
    }

    fn renew(&mut self, request: CoordinationRenewRequest, now: u64) -> CoordinationRenewPayload {
        let CoordinationRenewRequest {
            owner_id,
            nonce,
            lease_ttl_secs,
            operation,
        } = request;
        if nonce.is_empty() {
            return CoordinationRenewPayload {
                renewed: false,
                reason: Some("nonce must not be empty".into()),
            };
        }
        if owner_id.is_empty() {
            return CoordinationRenewPayload {
                renewed: false,
                reason: Some("owner_id must not be empty".into()),
            };
        }
        if lease_ttl_secs == 0 {
            return CoordinationRenewPayload {
                renewed: false,
                reason: Some("lease_ttl_secs must be greater than zero".into()),
            };
        }

        let key = operation_key(&operation);
        self.prune_expired_for_key(&key, now);
        let Some(existing) = self.prepared_by_key.get_mut(&key) else {
            return CoordinationRenewPayload {
                renewed: false,
                reason: Some("missing prepare for key".into()),
            };
        };

        if existing.owner_id != owner_id
            || existing.nonce != nonce
            || existing.operation != operation
        {
            return CoordinationRenewPayload {
                renewed: false,
                reason: Some("prepare does not match renew request".into()),
            };
        }
        existing.expires_at = now.saturating_add(lease_ttl_secs);
        CoordinationRenewPayload {
            renewed: true,
            reason: None,
        }
    }
}

impl DaemonState {
    pub(crate) async fn handle_coordination_prepare(
        &self,
        request: CoordinationPrepareRequest,
    ) -> DaemonResponse {
        match local_coordination_prepare(&self.coordination_ledger, request) {
            Ok(payload) => self.ok_with_payload(
                "coordination prepare accepted",
                Some(DaemonPayload::CoordinationPrepare(payload)),
            ),
            Err(LocalCoordinationError::Denied {
                code,
                message,
                payload,
            }) => self.err_with_payload(
                code,
                message,
                Some(DaemonPayload::CoordinationPrepare(payload)),
            ),
            Err(LocalCoordinationError::Lock(reason)) => {
                self.err("COORDINATION_LOCK_POISONED", reason)
            }
        }
    }

    pub(crate) async fn handle_coordination_commit(
        &self,
        request: CoordinationCommitRequest,
    ) -> DaemonResponse {
        let membership_store = self.active.as_ref().map(|active| active.store.membership());
        match local_coordination_commit(&self.coordination_ledger, membership_store, request).await
        {
            Ok(payload) => self.ok_with_payload(
                "coordination commit accepted",
                Some(DaemonPayload::CoordinationCommit(payload)),
            ),
            Err(LocalCoordinationCommitError::Denied {
                code,
                message,
                payload,
            }) => self.err_with_payload(
                code,
                message,
                Some(DaemonPayload::CoordinationCommit(payload)),
            ),
            Err(LocalCoordinationCommitError::Lock(reason)) => {
                self.err("COORDINATION_LOCK_POISONED", reason)
            }
        }
    }

    pub(crate) async fn handle_coordination_renew(
        &self,
        request: CoordinationRenewRequest,
    ) -> DaemonResponse {
        let now = now_unix_secs();
        let payload = match self.coordination_ledger.lock() {
            Ok(mut ledger) => ledger.renew(request, now),
            Err(err) => return self.err("COORDINATION_LOCK_POISONED", format!("{err}")),
        };
        if !payload.renewed {
            return self.err_with_payload(
                "COORDINATION_RENEW_DENIED",
                payload
                    .reason
                    .clone()
                    .unwrap_or_else(|| "coordination renew denied".into()),
                Some(DaemonPayload::CoordinationRenew(payload)),
            );
        }
        self.ok_with_payload(
            "coordination renew accepted",
            Some(DaemonPayload::CoordinationRenew(payload)),
        )
    }

    pub(crate) async fn handle_coordination_abort(
        &self,
        request: CoordinationAbortRequest,
    ) -> DaemonResponse {
        let now = now_unix_secs();
        match self.coordination_ledger.lock() {
            Ok(mut ledger) => ledger.abort(request, now),
            Err(err) => return self.err("COORDINATION_LOCK_POISONED", format!("{err}")),
        }
        self.ok("coordination abort accepted")
    }
}

fn key_tag(key: &CoordinationLockKey) -> String {
    match key {
        CoordinationLockKey::DeployNamespace { namespace } => format!("deploy:{namespace}"),
        CoordinationLockKey::SubnetClaim { subnet } => format!("subnet:{subnet}"),
        CoordinationLockKey::Membership { machine_id } => format!("membership:{machine_id}"),
    }
}

fn operation_key(operation: &CoordinationOperation) -> String {
    match operation {
        CoordinationOperation::LockAcquire { key } => key_tag(key),
        CoordinationOperation::SubnetClaimPrepare { subnet, .. }
        | CoordinationOperation::SubnetClaimCommit { subnet, .. }
        | CoordinationOperation::SubnetClaimAbort { subnet, .. } => {
            key_tag(&CoordinationLockKey::SubnetClaim {
                subnet: subnet.clone(),
            })
        }
        CoordinationOperation::MembershipPrepare { machine_id, .. }
        | CoordinationOperation::MembershipCommit { machine_id, .. }
        | CoordinationOperation::MembershipAbort { machine_id, .. } => {
            key_tag(&CoordinationLockKey::Membership {
                machine_id: machine_id.clone(),
            })
        }
    }
}

fn commit_matches_prepared_operation(
    prepared: &CoordinationOperation,
    commit: &CoordinationOperation,
) -> bool {
    match (prepared, commit) {
        (
            CoordinationOperation::LockAcquire { key: prepared_key },
            CoordinationOperation::LockAcquire { key: commit_key },
        ) => prepared_key == commit_key,
        (
            CoordinationOperation::SubnetClaimPrepare {
                machine_id: prepared_machine_id,
                subnet: prepared_subnet,
            },
            CoordinationOperation::SubnetClaimCommit {
                machine_id: commit_machine_id,
                subnet: commit_subnet,
            },
        ) => prepared_machine_id == commit_machine_id && prepared_subnet == commit_subnet,
        (
            CoordinationOperation::SubnetClaimCommit {
                machine_id: prepared_machine_id,
                subnet: prepared_subnet,
            },
            CoordinationOperation::SubnetClaimCommit {
                machine_id: commit_machine_id,
                subnet: commit_subnet,
            },
        ) => prepared_machine_id == commit_machine_id && prepared_subnet == commit_subnet,
        (
            CoordinationOperation::MembershipPrepare {
                machine_id: prepared_machine_id,
                invite_id: prepared_invite_id,
                ..
            },
            CoordinationOperation::MembershipCommit {
                machine_id: commit_machine_id,
                invite_id: commit_invite_id,
            },
        ) => prepared_machine_id == commit_machine_id && prepared_invite_id == commit_invite_id,
        (
            CoordinationOperation::LockAcquire { .. }
            | CoordinationOperation::SubnetClaimPrepare { .. }
            | CoordinationOperation::SubnetClaimCommit { .. }
            | CoordinationOperation::SubnetClaimAbort { .. }
            | CoordinationOperation::MembershipPrepare { .. }
            | CoordinationOperation::MembershipCommit { .. }
            | CoordinationOperation::MembershipAbort { .. },
            _,
        ) => false,
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time should be after unix epoch")
        .as_secs()
}

pub(crate) enum LocalCoordinationError {
    Denied {
        code: &'static str,
        message: String,
        payload: CoordinationPreparePayload,
    },
    Lock(String),
}

pub(crate) fn local_coordination_prepare(
    ledger: &std::sync::Mutex<CoordinationLedger>,
    request: CoordinationPrepareRequest,
) -> Result<CoordinationPreparePayload, LocalCoordinationError> {
    if let CoordinationOperation::MembershipPrepare {
        machine_id,
        invite_id,
        record,
        attestation,
    } = &request.operation
    {
        if record.id.0 != *machine_id {
            return Err(LocalCoordinationError::Denied {
                code: "COORDINATION_DENIED",
                message: "membership prepare: record machine_id mismatch".to_string(),
                payload: CoordinationPreparePayload {
                    accepted: false,
                    reason: Some("record machine_id mismatch".into()),
                    prepare_token: None,
                },
            });
        }
        if let Err(reason) = super::mesh::verify_membership_attestation(
            record,
            invite_id,
            &attestation.challenge_nonce,
            attestation,
        ) {
            return Err(LocalCoordinationError::Denied {
                code: "COORDINATION_DENIED",
                message: format!("membership attestation rejected: {reason}"),
                payload: CoordinationPreparePayload {
                    accepted: false,
                    reason: Some(reason),
                    prepare_token: None,
                },
            });
        }
    }

    let now = now_unix_secs();
    let payload = match ledger.lock() {
        Ok(mut ledger) => ledger.prepare(request, now),
        Err(err) => return Err(LocalCoordinationError::Lock(err.to_string())),
    };

    if !payload.accepted {
        let reason = payload
            .reason
            .clone()
            .unwrap_or_else(|| "coordination denied".into());
        return Err(LocalCoordinationError::Denied {
            code: "COORDINATION_DENIED",
            message: reason,
            payload,
        });
    }
    Ok(payload)
}

pub(crate) enum LocalCoordinationCommitError {
    Denied {
        code: &'static str,
        message: String,
        payload: CoordinationCommitPayload,
    },
    Lock(String),
}

pub(crate) async fn local_coordination_commit(
    ledger: &std::sync::Mutex<CoordinationLedger>,
    membership_store: Option<std::sync::Arc<dyn ployz_store_api::MembershipCommitStore>>,
    request: CoordinationCommitRequest,
) -> Result<CoordinationCommitPayload, LocalCoordinationCommitError> {
    let now = now_unix_secs();
    let (payload, prepared_op) = match ledger.lock() {
        Ok(mut ledger) => ledger.commit(request, now),
        Err(err) => return Err(LocalCoordinationCommitError::Lock(err.to_string())),
    };

    if !payload.committed {
        let reason = payload
            .reason
            .clone()
            .unwrap_or_else(|| "coordination commit denied".into());
        return Err(LocalCoordinationCommitError::Denied {
            code: "COORDINATION_COMMIT_DENIED",
            message: reason,
            payload,
        });
    }

    if let Some(CoordinationOperation::MembershipPrepare {
        invite_id, record, ..
    }) = prepared_op
    {
        let Some(store) = membership_store else {
            return Err(LocalCoordinationCommitError::Denied {
                code: "MEMBERSHIP_COMMIT_FAILED",
                message: "no active mesh available to apply membership commit".to_string(),
                payload: CoordinationCommitPayload {
                    committed: false,
                    reason: Some("no active mesh".into()),
                },
            });
        };
        if let Err(error) = store.commit_membership(&record, &invite_id, now).await {
            return Err(LocalCoordinationCommitError::Denied {
                code: "MEMBERSHIP_COMMIT_FAILED",
                message: format!("failed to apply membership commit: {error}"),
                payload: CoordinationCommitPayload {
                    committed: false,
                    reason: Some(format!("apply membership failed: {error}")),
                },
            });
        }
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::{CoordinationLedger, operation_key};
    use ployz_api::{
        CoordinationAbortRequest, CoordinationCommitRequest, CoordinationLockKey,
        CoordinationOperation, CoordinationPrepareRequest, CoordinationRenewRequest,
    };

    #[test]
    fn prepare_commit_round_trip_is_accepted() {
        let mut ledger = CoordinationLedger::default();
        let operation = CoordinationOperation::LockAcquire {
            key: CoordinationLockKey::DeployNamespace {
                namespace: "prod".into(),
            },
        };
        let prepare = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "n1".into(),
                lease_ttl_secs: 30,
                operation: operation.clone(),
            },
            100,
        );
        assert!(prepare.accepted);
        let Some(token) = prepare.prepare_token else {
            panic!("prepare token missing");
        };

        let commit = ledger.commit(
            CoordinationCommitRequest {
                owner_id: "founder-a".into(),
                nonce: "n1".into(),
                prepare_tokens: vec![token],
                operation,
            },
            101,
        );
        assert!(commit.0.committed);
    }

    #[test]
    fn prepare_renews_lease_for_same_nonce() {
        let mut ledger = CoordinationLedger::default();
        let operation = CoordinationOperation::SubnetClaimPrepare {
            machine_id: "m1".into(),
            subnet: "10.210.1.0/24".into(),
        };
        let accepted = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "n1".into(),
                lease_ttl_secs: 15,
                operation: operation.clone(),
            },
            10,
        );
        assert!(accepted.accepted);
        let Some(first_token) = accepted.prepare_token else {
            panic!("prepare token missing");
        };

        let renewed = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "n1".into(),
                lease_ttl_secs: 20,
                operation: operation.clone(),
            },
            11,
        );
        assert!(renewed.accepted);
        let Some(renewed_token) = renewed.prepare_token else {
            panic!("renewed token missing");
        };
        assert_eq!(first_token, renewed_token);
        let key = operation_key(&operation);
        let Some(prepared) = ledger.prepared_by_key.get(&key) else {
            panic!("prepared record missing");
        };
        assert_eq!(prepared.expires_at, 31);
    }

    #[test]
    fn abort_releases_prepared_key() {
        let mut ledger = CoordinationLedger::default();
        let operation = CoordinationOperation::SubnetClaimPrepare {
            machine_id: "m2".into(),
            subnet: "10.210.2.0/24".into(),
        };
        let prepare = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "nonce-a".into(),
                lease_ttl_secs: 10,
                operation: operation.clone(),
            },
            5,
        );
        assert!(prepare.accepted);

        ledger.abort(
            CoordinationAbortRequest {
                owner_id: "founder-a".into(),
                nonce: "nonce-a".into(),
                operation: operation.clone(),
            },
            6,
        );

        let retry = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "nonce-b".into(),
                lease_ttl_secs: 10,
                operation,
            },
            6,
        );
        assert!(retry.accepted);
    }

    #[test]
    fn renew_extends_existing_prepare_lease() {
        let mut ledger = CoordinationLedger::default();
        let operation = CoordinationOperation::LockAcquire {
            key: CoordinationLockKey::DeployNamespace {
                namespace: "preview".into(),
            },
        };
        let prepared = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "n-renew".into(),
                lease_ttl_secs: 5,
                operation: operation.clone(),
            },
            20,
        );
        assert!(prepared.accepted);
        let renewed = ledger.renew(
            CoordinationRenewRequest {
                owner_id: "founder-a".into(),
                nonce: "n-renew".into(),
                lease_ttl_secs: 30,
                operation: operation.clone(),
            },
            22,
        );
        assert!(renewed.renewed);
        let key = operation_key(&operation);
        let Some(record) = ledger.prepared_by_key.get(&key) else {
            panic!("renewed record missing");
        };
        assert_eq!(record.expires_at, 52);
    }

    #[test]
    fn commit_rejects_subnet_claim_with_different_machine_id() {
        let mut ledger = CoordinationLedger::default();
        let prepare_operation = CoordinationOperation::SubnetClaimPrepare {
            machine_id: "m-prepare".into(),
            subnet: "10.210.9.0/24".into(),
        };
        let prepare = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "nonce-1".into(),
                lease_ttl_secs: 20,
                operation: prepare_operation,
            },
            100,
        );
        assert!(prepare.accepted);
        let Some(token) = prepare.prepare_token else {
            panic!("prepare token missing");
        };

        let commit = ledger.commit(
            CoordinationCommitRequest {
                owner_id: "founder-a".into(),
                nonce: "nonce-1".into(),
                prepare_tokens: vec![token],
                operation: CoordinationOperation::SubnetClaimCommit {
                    machine_id: "m-other".into(),
                    subnet: "10.210.9.0/24".into(),
                },
            },
            101,
        );
        assert!(!commit.0.committed);
    }

    #[test]
    fn commit_accepts_subnet_claim_prepare_to_commit_with_same_identity() {
        let mut ledger = CoordinationLedger::default();
        let prepare = ledger.prepare(
            CoordinationPrepareRequest {
                owner_id: "founder-a".into(),
                nonce: "nonce-subnet".into(),
                lease_ttl_secs: 20,
                operation: CoordinationOperation::SubnetClaimPrepare {
                    machine_id: "m1".into(),
                    subnet: "10.210.20.0/24".into(),
                },
            },
            200,
        );
        assert!(prepare.accepted);
        let Some(token) = prepare.prepare_token else {
            panic!("prepare token missing");
        };

        let commit = ledger.commit(
            CoordinationCommitRequest {
                owner_id: "founder-a".into(),
                nonce: "nonce-subnet".into(),
                prepare_tokens: vec![token],
                operation: CoordinationOperation::SubnetClaimCommit {
                    machine_id: "m1".into(),
                    subnet: "10.210.20.0/24".into(),
                },
            },
            201,
        );
        assert!(commit.0.committed);
    }
}
