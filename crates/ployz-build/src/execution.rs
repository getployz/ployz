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
    Queued,
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
    /// Client-observed waiting and admitted execution, measured separately.
    Timing {
        queue_wait: Duration,
        execution: Duration,
    },
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
    /// Host-owned settings file, read once at admission.
    pub configuration_file: PathBuf,
    /// Host-installed Docker executable.
    pub docker: PathBuf,
    /// Total active budget beginning at admission.
    pub active_timeout: Duration,
    /// Maximum waiting attempts, excluding the active attempt.
    pub queue_capacity: usize,
    /// Waiting budget; execution starts a separate clock after admission.
    pub queue_timeout: Duration,
}
impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            state_directory: crate::builder::directory(),
            configuration_file: std::env::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".ployz/build.yaml"),
            docker: "docker".into(),
            active_timeout: EXECUTION_TIMEOUT,
            queue_capacity: 8,
            queue_timeout: Duration::from_secs(600),
        }
    }
}

impl HostPolicy {
    /// Read Machine-local settings once at daemon startup.
    /// # Errors
    /// Rejects non-numeric, zero timeout, and excessive settings.
    pub fn from_environment() -> Result<Self, BuildError> {
        Self::from_settings(|name| {
            std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
        })
    }

    fn from_settings(get: impl Fn(&str) -> Option<String>) -> Result<Self, BuildError> {
        let mut policy = Self::default();
        if let Some(raw) = get("PLOYZ_BUILD_QUEUE_CAPACITY") {
            policy.queue_capacity = raw.parse().map_err(|_| {
                BuildError::Prerequisite("invalid PLOYZ_BUILD_QUEUE_CAPACITY".into())
            })?;
        }
        for (name, value) in [
            (
                "PLOYZ_BUILD_QUEUE_TIMEOUT_SECONDS",
                &mut policy.queue_timeout,
            ),
            (
                "PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS",
                &mut policy.active_timeout,
            ),
        ] {
            if let Some(raw) = get(name) {
                *value = Duration::from_secs(
                    raw.parse()
                        .map_err(|_| BuildError::Prerequisite(format!("invalid {name}")))?,
                );
            }
        }
        policy.validate()?;
        Ok(policy)
    }

    /// Validate before allocating queue state or computing deadlines.
    /// # Errors
    /// Capacity is 0–1024; timeouts must be positive and at most 24 hours.
    pub fn validate(&self) -> Result<(), BuildError> {
        if self.queue_capacity > 1024
            || [self.queue_timeout, self.active_timeout]
                .iter()
                .any(|timeout| timeout.is_zero() || *timeout > Duration::from_secs(86400))
        {
            return Err(BuildError::Prerequisite("Build policy requires queue capacity 0–1024 and positive timeouts no greater than 86400 seconds".into()));
        }
        Ok(())
    }
}

/// Exclusive retained-builder ownership acquired before accepting source.
/// Dropping unused admission is safe: no execution has started.
pub struct Admission {
    pub(crate) lock: Lock,
    pub(crate) deadline: Deadline,
    pub(crate) cancellation: Cancellation,
    pub(crate) resources: crate::policy::Resources,
}
impl Admission {
    /// Attempt abandoned-resource teardown once after daemon restart. Never
    /// clears uncertainty about orphaned host processes or replays a Build.
    /// # Errors
    /// Busy or uncertain ownership remains unavailable.
    pub fn cleanup_abandoned(policy: &HostPolicy) -> Result<(), BuildError> {
        policy.validate()?;
        Lock::cleanup_abandoned(policy)
    }

    /// Receive into Machine-owned staging under this admission. Interrupted
    /// uploads from a previous daemon are removed before this path is reused.
    /// # Errors
    /// Refuses unsafe ownership or failure to remove/create protected staging.
    pub fn upload(self) -> Result<crate::remote::AdmittedUpload, crate::remote::InputError> {
        crate::upload::AdmittedUpload::new(self)
    }

    /// Acquire under host policy, including the deadline that starts before upload.
    /// # Errors
    /// Refuses busy or quarantined state and reports filesystem failures.
    pub fn try_acquire_with(policy: &HostPolicy) -> Result<Self, BuildError> {
        policy.validate()?;
        let resources = crate::policy::Resources::load(&policy.configuration_file)?;
        Ok(Self {
            resources,
            lock: Lock::try_acquire_in(&policy.state_directory)?,
            deadline: Deadline::starting_now(policy.active_timeout),
            cancellation: Cancellation::default(),
        })
    }
    pub(crate) fn wait() -> Result<Self, BuildError> {
        let lock = Lock::acquire()?;
        let resources = crate::policy::Resources::load(&HostPolicy::default().configuration_file)?;
        Ok(Self {
            resources,
            lock,
            deadline: Deadline::starting_now(EXECUTION_TIMEOUT),
            cancellation: Cancellation::default(),
        })
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
            return Err(BuildError::TimedOut(self.deadline.budget.as_secs()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_removes_abandoned_uploads_but_keeps_unknown_ownership_unavailable() {
        let root =
            std::env::temp_dir().join(format!("ployz-build-restart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let policy = HostPolicy {
            state_directory: root.clone(),
            docker: root.join("docker"),
            ..Default::default()
        };
        crate::tests::executable(
            &policy.docker,
            &format!(
                "#!/bin/sh\n[ \"$1\" = --ready ] && exit 0\nprintf cleaned > '{}/cleaned'\n",
                root.display()
            ),
        );
        std::fs::create_dir(root.join("build-upload")).unwrap();
        std::fs::write(root.join("build-upload/abandoned"), "private old capture").unwrap();
        Admission::cleanup_abandoned(&policy).unwrap();
        assert!(
            !root.join("build-upload").exists(),
            "startup left abandoned inputs"
        );
        let admission = Admission::try_acquire_with(&policy).unwrap();
        let upload = admission.upload().unwrap();
        assert!(!root.join("build-upload/abandoned").exists());
        assert!(matches!(
            Admission::try_acquire_with(&policy),
            Err(BuildError::Busy)
        ));
        drop(upload);
        let mut lock = Lock::try_acquire_in(&root).unwrap();
        lock.quarantine().unwrap();
        drop(lock);
        assert!(
            Admission::cleanup_abandoned(&policy)
                .unwrap_err()
                .is_unknown()
        );
        assert!(
            root.join("cleaned").exists(),
            "cleanup skipped absent upload staging"
        );
        std::fs::remove_file(root.join("cleaned")).unwrap();
        std::fs::create_dir(root.join("build-upload")).unwrap();
        std::fs::write(root.join("build-upload/uncertain"), "still in use").unwrap();
        assert!(
            Admission::cleanup_abandoned(&policy)
                .unwrap_err()
                .is_unknown()
        );
        assert!(
            root.join("cleaned").exists(),
            "bounded abandoned cleanup was not attempted"
        );
        assert!(
            root.join("build-upload/uncertain").exists(),
            "unknown work lost private inputs"
        );
        assert!(
            Admission::try_acquire_with(&policy)
                .err()
                .unwrap()
                .is_unknown()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expired_admission_and_docker_report_configured_budget() {
        let root = std::env::temp_dir().join(format!("ployz-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let policy = HostPolicy {
            state_directory: root.clone(),
            active_timeout: Duration::from_secs(5),
            ..Default::default()
        };
        let mut admission = Admission::try_acquire_with(&policy).unwrap();
        admission.deadline.expires = std::time::Instant::now();
        assert!(matches!(admission.check(), Err(BuildError::TimedOut(5))));
        let environment = std::collections::BTreeMap::new();
        let docker = crate::Docker {
            program: &policy.docker,
            environment: &environment,
            working_dir: &root,
            deadline: admission.deadline,
            cancellation: None,
            progress: None,
        };
        assert!(matches!(
            docker.run("build", &["build"], crate::Streams::Captured),
            Err(BuildError::TimedOut(5))
        ));
        drop(admission);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_admission_cannot_delete_or_create_upload_staging() {
        let root =
            std::env::temp_dir().join(format!("ployz-upload-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("build-upload")).unwrap();
        std::fs::write(root.join("build-upload/existing"), "capture").unwrap();
        let policy = HostPolicy {
            state_directory: root.clone(),
            ..Default::default()
        };
        let admission = Admission::try_acquire_with(&policy).unwrap();
        admission.cancellation().cancel();
        assert!(admission.upload().is_err());
        assert!(root.join("build-upload/existing").exists());
        assert!(Admission::try_acquire_with(&policy).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn machine_policy_defaults_and_invalid_input_are_bounded() {
        let policy = HostPolicy::from_settings(|_| None).unwrap();
        assert_eq!(policy.queue_capacity, 8);
        assert_eq!(policy.queue_timeout, Duration::from_secs(600));
        assert_eq!(policy.active_timeout, Duration::from_secs(1800));
        for (name, values) in [
            ("PLOYZ_BUILD_QUEUE_CAPACITY", vec!["-1", "1025", "abc"]),
            (
                "PLOYZ_BUILD_QUEUE_TIMEOUT_SECONDS",
                vec!["0", "86401", "18446744073709551615"],
            ),
            (
                "PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS",
                vec!["0", "86401", "invalid"],
            ),
        ] {
            for value in values {
                assert!(
                    HostPolicy::from_settings(|key| (key == name).then(|| value.into())).is_err(),
                    "{name}={value}"
                );
            }
        }
        let policy = HostPolicy::from_settings(|key| match key {
            "PLOYZ_BUILD_QUEUE_CAPACITY" => Some("0".into()),
            "PLOYZ_BUILD_QUEUE_TIMEOUT_SECONDS" => Some("12".into()),
            "PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS" => Some("34".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(policy.queue_capacity, 0);
        assert_eq!(policy.queue_timeout, Duration::from_secs(12));
        assert_eq!(policy.active_timeout, Duration::from_secs(34));
    }
}
