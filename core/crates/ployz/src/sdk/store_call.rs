//! Every Config Store call Cloud's SDK makes runs through [`store_call`]: its blocking
//! database work on a worker thread, at most [`CONCURRENCY`] at once in this process,
//! ending `unavailable` when it waits or runs too long.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::ConfigStore;
use tokio::sync::Semaphore;

/// Store calls running at once; more wait their turn.
const CONCURRENCY: usize = 8;
/// How long a call may wait for its turn.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a call may run. The database's own statement and lock timeouts end the
/// work itself; past this the caller stops waiting and the outcome is unknown.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

// ponytail: one bound per process, not per Store handle; Cloud opens one Store.
static PERMITS: Semaphore = Semaphore::const_new(CONCURRENCY);

/// Run `work` on `store` off the async threads, bounded and timed.
///
/// # Errors
/// Returns `work`'s error, or `unavailable` when the Store is too busy or slow.
pub async fn store_call<T: Send + 'static>(
    store: &Arc<ConfigStore>,
    work: impl FnOnce(&ConfigStore) -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let permit = tokio::time::timeout(QUEUE_TIMEOUT, PERMITS.acquire())
        .await
        .map_err(|_| unavailable("The Config Store is busy; retry"))?
        .map_err(|_| unavailable("The Config Store is closed"))?;
    let store = Arc::clone(store);
    let work = tokio::task::spawn_blocking(move || work(&store));
    let result = tokio::time::timeout(RUN_TIMEOUT, work).await;
    // The turn ends when the caller stops waiting; ponytail: work past RUN_TIMEOUT
    // runs on unbounded until the database's own timeouts end it.
    drop(permit);
    result
        .map_err(|_| {
            unavailable("The Config Store did not answer in time; the outcome is unknown")
        })?
        .map_err(|_| unavailable("The Config Store call stopped unexpectedly"))?
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.to_owned(),
        details: serde_json::Value::Null,
    }
}
