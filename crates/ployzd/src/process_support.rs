//! Shared scaffolding for the role processes: shutdown signal,
//! failure backoff, attempt recording, and lazily opened handles.

use std::future::Future;
use std::time::Duration;

use ployz_nats::service_runtime::NatsServiceShutdownError;
use tokio::sync::{broadcast, mpsc};
use tokio::task::AbortHandle;

/// Runs one role's cleanup within a total deadline and force-cancels its
/// remaining tasks if the deadline expires.
pub async fn bounded_role_shutdown(
    role: &str,
    deadline: Duration,
    abort_handles: &[AbortHandle],
    cleanup: impl Future<Output = Result<(), NatsServiceShutdownError>>,
) -> Result<(), NatsServiceShutdownError> {
    match tokio::time::timeout(deadline, cleanup).await {
        Ok(result) => result,
        Err(_) => {
            for handle in abort_handles {
                handle.abort();
            }
            tracing::warn!(
                role,
                deadline_seconds = deadline.as_secs(),
                "shutdown cleanup exceeded its deadline; forcing remaining tasks"
            );
            Ok(())
        }
    }
}

/// Registers shutdown handlers immediately and returns their waiter.
pub fn shutdown_signal() -> Result<impl Future<Output = Result<(), std::io::Error>>, std::io::Error>
{
    #[cfg(unix)]
    {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        Ok(async move {
            tokio::select! {
                _ = interrupt.recv() => Ok(()),
                _ = terminate.recv() => Ok(()),
            }
        })
    }
    #[cfg(not(unix))]
    {
        Ok(async { tokio::signal::ctrl_c().await })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshDelay {
    Elapsed,
    Woken,
    WakeClosed,
    Shutdown,
}

pub fn drain_refresh_wakes(receiver: &mut mpsc::Receiver<()>) {
    while receiver.try_recv().is_ok() {}
}

pub async fn wait_for_refresh_delay(
    duration: Duration,
    wake: &mut mpsc::Receiver<()>,
    shutdown: &mut broadcast::Receiver<()>,
) -> RefreshDelay {
    tokio::select! {
        () = tokio::time::sleep(duration) => RefreshDelay::Elapsed,
        wake = wake.recv() => {
            if wake.is_some() {
                RefreshDelay::Woken
            } else {
                RefreshDelay::WakeClosed
            }
        }
        _ = shutdown.recv() => RefreshDelay::Shutdown,
    }
}

pub async fn sleep_or_shutdown(duration: Duration, shutdown: &mut broadcast::Receiver<()>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        _ = shutdown.recv() => true,
    }
}

/// The steady polling interval plus the cap for exponential failure backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffSchedule {
    pub interval: Duration,
    pub cap: Duration,
}

impl BackoffSchedule {
    #[must_use]
    pub fn next_after_failure(&self, current_backoff: Duration) -> Duration {
        current_backoff.saturating_mul(2).min(self.cap)
    }
}

/// One recorded outcome of a periodic attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedAttempt<A> {
    /// Resets the failure streak and returns to the steady interval.
    Healthy(A),
    /// Counts toward the failure streak but keeps the steady interval
    /// (e.g. serving last-known-good data while the source is away).
    Degraded(A),
    /// Counts toward the failure streak and backs off exponentially.
    Failed(A),
}

/// Records the attempt into `(last_attempt, consecutive_failures)` and
/// returns the next sleep duration.
pub fn record_attempt<A>(
    last_attempt: &mut Option<A>,
    consecutive_failures: &mut u64,
    attempt: RecordedAttempt<A>,
    schedule: BackoffSchedule,
    current_backoff: Duration,
) -> Duration {
    match attempt {
        RecordedAttempt::Healthy(attempt) => {
            *last_attempt = Some(attempt);
            *consecutive_failures = 0;
            schedule.interval
        }
        RecordedAttempt::Degraded(attempt) => {
            *last_attempt = Some(attempt);
            *consecutive_failures += 1;
            schedule.interval
        }
        RecordedAttempt::Failed(attempt) => {
            *last_attempt = Some(attempt);
            *consecutive_failures += 1;
            schedule.next_after_failure(current_backoff)
        }
    }
}

/// A handle that is opened on first use and reused afterwards. Opening
/// failures are returned to the caller and retried on the next use.
#[derive(Debug)]
pub struct LazyHandle<T> {
    value: Option<T>,
}

impl<T> Default for LazyHandle<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> LazyHandle<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self { value: None }
    }

    /// Returns the opened value, opening it first on first use.
    pub async fn get_or_open<E>(
        &mut self,
        open: impl AsyncFnOnce() -> Result<T, E>,
    ) -> Result<&T, E> {
        let value = match self.value.take() {
            Some(value) => value,
            None => open().await?,
        };
        Ok(self.value.insert(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_backoff_doubles_and_caps() {
        let schedule = BackoffSchedule {
            interval: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        };

        assert_eq!(
            schedule.next_after_failure(Duration::from_secs(2)),
            Duration::from_secs(4)
        );
        assert_eq!(
            schedule.next_after_failure(Duration::from_secs(20)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn degraded_attempt_counts_failure_but_keeps_steady_interval() {
        let schedule = BackoffSchedule {
            interval: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        };
        let mut last_attempt = None;
        let mut consecutive_failures = 0;

        let next = record_attempt(
            &mut last_attempt,
            &mut consecutive_failures,
            RecordedAttempt::Degraded("last-known-good"),
            schedule,
            Duration::from_secs(8),
        );

        assert_eq!(next, Duration::from_secs(1));
        assert_eq!(last_attempt, Some("last-known-good"));
        assert_eq!(consecutive_failures, 1);
    }

    #[tokio::test]
    async fn refresh_delay_reports_wake_and_shutdown() {
        let (wake_tx, mut wake_rx) = mpsc::channel(1);
        let (shutdown_tx, mut shutdown_rx) = broadcast::channel(1);

        wake_tx.send(()).await.expect("wake sends");
        assert_eq!(
            wait_for_refresh_delay(Duration::from_secs(30), &mut wake_rx, &mut shutdown_rx).await,
            RefreshDelay::Woken
        );

        shutdown_tx.send(()).expect("shutdown sends");
        assert_eq!(
            wait_for_refresh_delay(Duration::from_secs(30), &mut wake_rx, &mut shutdown_rx).await,
            RefreshDelay::Shutdown
        );
    }

    #[tokio::test]
    async fn sleep_or_shutdown_returns_true_on_shutdown() {
        let (shutdown_tx, mut shutdown_rx) = broadcast::channel(1);
        shutdown_tx.send(()).expect("shutdown sends");

        assert!(sleep_or_shutdown(Duration::from_secs(30), &mut shutdown_rx).await);
    }

    #[tokio::test]
    async fn bounded_role_shutdown_aborts_tasks_after_deadline() {
        let task = tokio::spawn(std::future::pending::<()>());
        let abort_handles = [task.abort_handle()];

        bounded_role_shutdown(
            "test",
            Duration::ZERO,
            &abort_handles,
            std::future::pending(),
        )
        .await
        .expect("forced shutdown succeeds");

        assert!(
            task.await
                .expect_err("task is aborted after the deadline")
                .is_cancelled()
        );
    }

    #[tokio::test]
    async fn lazy_handle_opens_once_and_retries_after_failure() {
        let mut handle = LazyHandle::<u32>::new();
        let failed: Result<&u32, &str> = handle.get_or_open(async || Err("unavailable")).await;
        assert_eq!(failed, Err("unavailable"));

        let opened = handle
            .get_or_open(async || Ok::<u32, &str>(7))
            .await
            .expect("handle opens");
        assert_eq!(*opened, 7);

        let reused = handle
            .get_or_open(async || Err("must not reopen"))
            .await
            .expect("handle is reused");
        assert_eq!(*reused, 7);
    }
}
