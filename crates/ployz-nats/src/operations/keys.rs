use ployz_core::ids::OperationId;
use ployz_core::machine::JoinTokenFingerprint;
use ployz_core::ops::OperationIdempotencyKey;

#[must_use]
pub fn operation_status_key(operation_id: &OperationId) -> String {
    format!("ops.{}", operation_id.as_str())
}

pub const MACHINE_ADD_SUBMISSION_KEY_PREFIX: &str = "machine_add_submissions.";
const MACHINE_ADD_CLAIM_KEY_PREFIX: &str = "machine_add_claims.";

#[must_use]
pub fn machine_add_submission_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!(
        "{MACHINE_ADD_SUBMISSION_KEY_PREFIX}{}",
        idempotency_key.as_str()
    )
}

#[must_use]
pub fn machine_add_claim_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!("{MACHINE_ADD_CLAIM_KEY_PREFIX}{}", idempotency_key.as_str())
}

#[must_use]
pub fn machine_add_secret_delivery_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!("machine_add_secret_deliveries.{}", idempotency_key.as_str())
}

#[must_use]
pub fn machine_add_mint_claim_key(idempotency_key: &OperationIdempotencyKey) -> String {
    format!("machine_add_mint_claims.{}", idempotency_key.as_str())
}

#[must_use]
pub fn machine_add_join_token_key(fingerprint: &JoinTokenFingerprint) -> String {
    format!("machine_add_join_tokens.{}", fingerprint.as_str())
}
