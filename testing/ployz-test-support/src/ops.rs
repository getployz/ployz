//! Generic bounded polling for asynchronous tests.

use std::time::Duration;

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
