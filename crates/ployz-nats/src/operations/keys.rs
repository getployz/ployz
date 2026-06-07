use ployz_core::ids::OperationId;
use ployz_core::machine::JoinTokenFingerprint;
use ployz_core::ops::OperationIdempotencyKey;

#[must_use]
pub fn operation_status_key(operation_id: &OperationId) -> String {
    format!("ops.{}", operation_id.as_str())
}

#[must_use]
pub fn operation_owner_lease_key(operation_id: &OperationId) -> String {
    format!("operation_leases.{}", operation_id.as_str())
}

#[must_use]
pub fn deploy_submission_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!("deploy_submissions.{}", idempotency_key.as_str())
}

#[must_use]
pub fn cert_submission_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!("cert_submissions.{}", idempotency_key.as_str())
}

#[must_use]
pub fn machine_add_submission_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!("machine_add_submissions.{}", idempotency_key.as_str())
}

#[must_use]
pub fn machine_add_join_token_key(fingerprint: &JoinTokenFingerprint) -> String {
    format!("machine_add_join_tokens.{}", fingerprint.as_str())
}
