//! Waits on state a real Cluster exposes only to polling: Docker, daemons, proxies, DNS.

use std::{fmt, time::Duration};

use tokio::time::Instant;

const INTERVAL: Duration = Duration::from_millis(250);

/// One look at the Cluster: `Ok` when the condition holds, otherwise what was seen instead.
pub trait Attempt {
    type Output;

    /// # Errors
    ///
    /// Returns what was observed, if anything, when the condition does not hold.
    fn into_result(self) -> Result<Self::Output, Option<String>>;
}

impl Attempt for bool {
    type Output = ();

    fn into_result(self) -> Result<(), Option<String>> {
        if self { Ok(()) } else { Err(None) }
    }
}

impl<T> Attempt for Option<T> {
    type Output = T;

    fn into_result(self) -> Result<T, Option<String>> {
        self.ok_or(None)
    }
}

impl<T, E: fmt::Display> Attempt for Result<T, E> {
    type Output = T;

    fn into_result(self) -> Result<T, Option<String>> {
        self.map_err(|observed| Some(observed.to_string()))
    }
}

/// A condition that did not arrive in time, or did not hold for its window.
#[derive(thiserror::Error)]
#[error("{0}")]
pub struct WaitError(String);

// Debug shows the message so a test's `unwrap` prints the last observation unescaped.
impl fmt::Debug for WaitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Probe until the condition holds, then return what the probe produced.
///
/// # Errors
///
/// Fails when `timeout` passes first, naming `what` and the last observation.
pub async fn poll_until<A: Attempt>(
    timeout: Duration,
    what: &str,
    mut probe: impl AsyncFnMut() -> A,
) -> Result<A::Output, WaitError> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    loop {
        match tokio::time::timeout_at(deadline, probe()).await {
            Ok(attempt) => match attempt.into_result() {
                Ok(output) => return Ok(output),
                Err(observed) => last = observed,
            },
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        tick(deadline).await;
    }
    Err(WaitError(match last {
        Some(observed) => format!("{what}: not reached within {timeout:?}; last saw:\n{observed}"),
        None => format!("{what}: not reached within {timeout:?}"),
    }))
}

/// Probe for the whole `window` and fail as soon as the condition stops holding.
///
/// Asserts that something does not happen: the last probe runs once `window` has passed.
///
/// # Errors
///
/// Fails on the first probe that does not hold, naming `what` and the observation.
pub async fn hold_for<A: Attempt>(
    window: Duration,
    what: &str,
    mut probe: impl AsyncFnMut() -> A,
) -> Result<(), WaitError> {
    let start = Instant::now();
    let end = start + window;
    loop {
        if let Err(observed) = probe().await.into_result() {
            let elapsed = start.elapsed();
            return Err(WaitError(match observed {
                Some(observed) => format!("{what}: broke after {elapsed:?}:\n{observed}"),
                None => format!("{what}: broke after {elapsed:?}"),
            }));
        }
        if Instant::now() >= end {
            return Ok(());
        }
        tick(end).await;
    }
}

async fn tick(until: Instant) {
    // real-wait: the Cluster runs in Docker containers and real processes that emit no event a test can await
    tokio::time::sleep_until((Instant::now() + INTERVAL).min(until)).await;
}
