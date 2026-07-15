//! Shipped protocol and runtime-evidence contracts exercised by DinD tests.

use std::time::Duration;

use ployz_core::state::IntentSnapshot;
use ployz_core::subjects::INTENT_GET;
use ployz_nats::service_runtime::{NatsJsonServiceRequestError, request_json};

pub const MANAGED_LABEL: &str = "plz.managed";
pub const NAMESPACE_ID_LABEL: &str = "plz.namespace_id";
pub const SERVICE_ID_LABEL: &str = "plz.service_id";
pub const NAMESPACE_REVISION_ENTRY_LABEL: &str = "plz.namespace_revision_entry";
pub const OPERATION_ID_LABEL: &str = "plz.operation_id";
pub const CONTAINER_TYPE_LABEL: &str = "plz.container_type";

/// Reads the complete operator-intent projection through its public NATS
/// request/reply contract. The empty JSON object is the `intent.get` wire
/// request; E2E deliberately does not import the daemon's client wrapper.
pub async fn read_intent(
    client: &async_nats::Client,
    timeout: Duration,
) -> Result<IntentSnapshot, NatsJsonServiceRequestError> {
    request_json(
        client,
        INTENT_GET.to_owned(),
        &serde_json::json!({}),
        timeout,
    )
    .await
}
