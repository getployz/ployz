//! Bounded Corrosion-backed read lenses for the API role.
//!
//! Corrosion subscriptions are wake signals. This module never promotes a
//! subscription delta to lens truth; it uses the bounded client to load and
//! validate the complete scoped rows before publishing a state.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::ids::ClusterName;
use ployz_core::{ApiRefusal, LensCollection, LensSnapshot};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::corrosion::CorrosionClient;

mod actor;
mod adapter;
mod snapshots;

#[cfg(test)]
mod tests;

/// The fixed scope and recovery bound of one local API lens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensEngineConfig {
    cluster_id: ClusterName,
    recovery: LensRecoveryPolicy,
    limits: LensLimits,
}

impl LensEngineConfig {
    #[must_use]
    pub fn new(cluster_id: ClusterName, recovery: LensRecoveryPolicy) -> Self {
        Self {
            cluster_id,
            recovery,
            limits: LensLimits::production(),
        }
    }

    #[must_use]
    pub fn cluster_id(&self) -> &ClusterName {
        &self.cluster_id
    }

    #[must_use]
    pub const fn recovery(&self) -> LensRecoveryPolicy {
        self.recovery
    }

    pub(super) const fn limits(&self) -> LensLimits {
        self.limits
    }

    #[cfg(test)]
    pub(super) fn with_limits(mut self, limits: LensLimits) -> Self {
        self.limits = limits;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LensLimits {
    query_timeout: Duration,
    initial_snapshot_timeout: Duration,
    max_rows: usize,
}

impl LensLimits {
    const fn production() -> Self {
        Self {
            query_timeout: Duration::from_secs(5),
            initial_snapshot_timeout: Duration::from_secs(5),
            max_rows: 10_000,
        }
    }

    #[cfg(test)]
    pub(super) const fn for_test(
        query_timeout: Duration,
        initial_snapshot_timeout: Duration,
        max_rows: usize,
    ) -> Self {
        Self {
            query_timeout,
            initial_snapshot_timeout,
            max_rows,
        }
    }

    pub(super) const fn query_timeout(self) -> Duration {
        self.query_timeout
    }

    pub(super) const fn initial_snapshot_timeout(self) -> Duration {
        self.initial_snapshot_timeout
    }

    pub(super) const fn max_rows(self) -> usize {
        self.max_rows
    }
}

/// A finite exponential-retry budget for one interruption episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LensRecoveryPolicy {
    max_attempts: NonZeroU32,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl LensRecoveryPolicy {
    pub fn try_new(
        max_attempts: NonZeroU32,
        initial_backoff: Duration,
        max_backoff: Duration,
    ) -> Result<Self, LensRecoveryPolicyError> {
        if initial_backoff.is_zero() {
            return Err(LensRecoveryPolicyError::ZeroInitialBackoff);
        }
        if max_backoff < initial_backoff {
            return Err(LensRecoveryPolicyError::BackoffCapBeforeInitial {
                initial_backoff,
                max_backoff,
            });
        }
        Ok(Self {
            max_attempts,
            initial_backoff,
            max_backoff,
        })
    }

    #[must_use]
    pub const fn max_attempts(self) -> NonZeroU32 {
        self.max_attempts
    }

    #[must_use]
    pub const fn initial_backoff(self) -> Duration {
        self.initial_backoff
    }

    #[must_use]
    pub const fn max_backoff(self) -> Duration {
        self.max_backoff
    }
}

/// An invalid bounded-recovery configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LensRecoveryPolicyError {
    #[error("lens recovery requires a nonzero initial backoff")]
    ZeroInitialBackoff,
    #[error(
        "lens recovery backoff cap {max_backoff:?} is shorter than initial backoff {initial_backoff:?}"
    )]
    BackoffCapBeforeInitial {
        initial_backoff: Duration,
        max_backoff: Duration,
    },
}

/// A live local view of one Core lens collection.
///
/// The receiver is a state cell: a slow consumer receives the newest complete
/// value rather than an unbounded queue of obsolete states. A successful state
/// is an `Ok` snapshot. A domain-terminal refusal remains the final `Err`
/// value after its worker exits; recovery exhaustion publishes
/// `CorrosionUnavailable` before the API role exits for supervisor restart.
pub struct LensWatch {
    receiver: watch::Receiver<Option<Result<LensSnapshot, ApiRefusal>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl LensWatch {
    /// Clones the current-value receiver for one downstream adapter.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<Result<LensSnapshot, ApiRefusal>>> {
        self.receiver.clone()
    }

    /// Cancels the local Corrosion worker and waits for it to stop.
    pub async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

/// Starts one bounded Corrosion-backed state lens.
#[must_use]
pub fn start_lens(
    client: CorrosionClient,
    collection: LensCollection,
    config: LensEngineConfig,
) -> LensWatch {
    let (lifecycle_failures, _receiver) = mpsc::unbounded_channel();
    actor::start_lens_with_store(Arc::new(client), collection, config, lifecycle_failures)
}

/// Starts one bounded Corrosion-backed state lens owned by the API role.
#[must_use]
pub(super) fn start_lens_with_lifecycle(
    client: CorrosionClient,
    collection: LensCollection,
    config: LensEngineConfig,
    lifecycle_failures: mpsc::UnboundedSender<LensCollection>,
) -> LensWatch {
    actor::start_lens_with_store(Arc::new(client), collection, config, lifecycle_failures)
}
