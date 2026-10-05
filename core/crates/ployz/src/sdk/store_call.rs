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
/// How long a caller waits for a call. The database's own statement and lock
/// timeouts end the work itself; past this the outcome is unknown.
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
    let store = Arc::clone(store);
    bounded(&PERMITS, QUEUE_TIMEOUT, RUN_TIMEOUT, move || work(&store)).await
}

async fn bounded<T: Send + 'static>(
    permits: &'static Semaphore,
    queue: Duration,
    run: Duration,
    work: impl FnOnce() -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let permit = tokio::time::timeout(queue, permits.acquire())
        .await
        .map_err(|_| unavailable("The Config Store is busy; retry"))?
        .map_err(|_| unavailable("The Config Store is closed"))?;
    // The work holds its turn until it ends, even after its caller stops waiting,
    // so no more than the bound ever runs at once.
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    });
    tokio::time::timeout(run, work)
        .await
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
        cause: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_call_waiting_past_its_turn_is_busy() {
        static ONE: Semaphore = Semaphore::const_new(1);
        let _held = ONE.acquire().await.unwrap();
        let error = bounded(&ONE, Duration::from_millis(10), RUN_TIMEOUT, || Ok(()))
            .await
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert!(error.message.contains("busy"), "{}", error.message);
    }

    #[tokio::test]
    async fn a_slow_call_times_out_and_keeps_its_turn_until_it_ends() {
        static ONE: Semaphore = Semaphore::const_new(1);
        let (done, finished) = std::sync::mpsc::channel();
        let error = bounded(&ONE, QUEUE_TIMEOUT, Duration::from_millis(10), move || {
            std::thread::sleep(Duration::from_millis(200));
            done.send(()).unwrap();
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(error.message.contains("in time"), "{}", error.message);
        assert_eq!(
            ONE.available_permits(),
            0,
            "the running work keeps its turn"
        );
        finished.recv().unwrap();
        let waited = tokio::time::timeout(Duration::from_secs(5), ONE.acquire()).await;
        assert!(waited.is_ok(), "the turn ends with the work");
    }
}
