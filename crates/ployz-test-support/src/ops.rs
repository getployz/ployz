//! Operation polling helpers: one terminal-status wait for every operation
//! kind instead of per-kind copies.

use std::time::Duration;

use ployz_core::ids::OperationId;
use ployz_core::ops::OperationStatus;
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::OpsStatusRequest;

const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Polls `probe` every `interval` until it yields `Some`, within `budget`.
pub async fn poll_until<T>(
    budget: Duration,
    interval: Duration,
    mut probe: impl AsyncFnMut() -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if let Some(value) = probe().await {
            return Some(value);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Polls the operation status API until the operation is terminal, within
/// the budget. Panics with the last error/status when the budget runs out.
pub async fn wait_for_terminal_status(
    api: &OperationApiClient,
    operation_id: &OperationId,
    budget: Duration,
) -> OperationStatus {
    let terminal = poll_until(budget, STATUS_POLL_INTERVAL, async || {
        let status = api
            .ops_status(&OpsStatusRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .expect("status is readable")
            .status;
        status.is_terminal().then_some(status)
    })
    .await;

    let Some(status) = terminal else {
        panic!("operation {operation_id:?} did not reach terminal status within {budget:?}");
    };
    status
}
