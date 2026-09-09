//! Admission and connection-scoped cancellation for the shared host executor.

use crate::{BuildError, Deadline, EXECUTION_TIMEOUT, builder::Lock};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Earliest observed phase of one Build attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Stage {
    Admission,
    Upload,
    Preparation,
    Building,
    Output,
    Cleanup,
}

/// Bounded output and work observations from the execution host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Progress {
    Stage(Stage),
    Output(Vec<u8>),
    /// Proven per-target work, retained even when a later step fails.
    Target {
        name: String,
        outcome: TargetEvidence,
    },
}

/// What this attempt proved about one target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TargetEvidence {
    Unattempted,
    Unknown,
    Image(crate::BuiltImage),
    Validated,
    Published,
}

/// Per-target evidence accumulated independently of terminal success.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkEvidence(pub std::collections::BTreeMap<String, TargetEvidence>);
impl WorkEvidence {
    /// Initially none of the admitted targets has been attempted.
    #[must_use]
    pub fn new(targets: &[crate::Target]) -> Self {
        Self(
            targets
                .iter()
                .map(|target| (target.name.clone(), TargetEvidence::Unattempted))
                .collect(),
        )
    }
    /// Record only observed progress; starting Bake does not prove completion.
    pub fn observe(&mut self, event: &Progress) {
        if let Progress::Target { name, outcome } = event {
            self.0.insert(name.clone(), outcome.clone());
        }
    }
}

/// A request to stop, never proof that execution stopped.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    /// Request bounded termination.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    /// Whether termination has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Execution-host settings. These are never deserialized from a Build request.
#[derive(Clone)]
pub struct HostPolicy {
    /// Machine-local retained builder ownership.
    pub state_directory: PathBuf,
    /// Host-installed Docker executable.
    pub docker: PathBuf,
    /// Total active budget beginning at admission.
    pub active_timeout: Duration,
}
impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            state_directory: crate::builder::directory(),
            docker: "docker".into(),
            active_timeout: EXECUTION_TIMEOUT,
        }
    }
}

/// Exclusive retained-builder ownership acquired before accepting source.
/// Dropping unused admission is safe: no execution has started.
pub struct Admission {
    pub(crate) lock: Lock,
    pub(crate) deadline: Deadline,
    pub(crate) cancellation: Cancellation,
}
impl Admission {
    /// Acquire under host policy, including the deadline that starts before upload.
    /// # Errors
    /// Refuses busy or quarantined state and reports filesystem failures.
    pub fn try_acquire_with(policy: &HostPolicy) -> Result<Self, BuildError> {
        Ok(Self {
            lock: Lock::try_acquire_in(&policy.state_directory)?,
            deadline: Deadline::starting_now(policy.active_timeout),
            cancellation: Cancellation::default(),
        })
    }
    pub(crate) fn wait() -> Result<Self, BuildError> {
        Ok(Self::new(Lock::acquire()?))
    }
    fn new(lock: Lock) -> Self {
        Self {
            lock,
            deadline: Deadline::starting_now(EXECUTION_TIMEOUT),
            cancellation: Cancellation::default(),
        }
    }
    /// Handle for requesting this attempt to stop.
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    /// Active budget still available to this attempt.
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.deadline.remaining()
    }
    /// # Errors
    /// Returns cancellation or timeout before starting more work.
    pub fn check(&self) -> Result<(), BuildError> {
        if self.cancellation.is_cancelled() {
            return Err(BuildError::Cancelled);
        }
        if self.remaining().is_zero() {
            return Err(BuildError::TimedOut(EXECUTION_TIMEOUT.as_secs()));
        }
        Ok(())
    }
}
