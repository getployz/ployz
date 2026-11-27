//! Admission and connection-scoped cancellation for the shared host executor.

use crate::{BuildError, Deadline, EXECUTION_TIMEOUT, builder::Lock};
use ployz_core::BuildConcurrency;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
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
    /// Pushing the built image out of the host, after the Build itself (`ployz build`).
    Push,
}

/// Bounded output and work observations from the execution host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Progress {
    Stage(Stage),
    /// Output the execution host could not attribute to a step.
    Output(Vec<u8>),
    /// A BuildKit step, repeated whenever its state changes.
    Step(BuildStep),
    /// Output BuildKit attributed to one step.
    StepOutput {
        step: String,
        stderr: bool,
        text: String,
    },
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

/// One BuildKit vertex: a Dockerfile instruction, image resolution, or
/// context transfer. `id` is stable across repeated reports of the same step.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuildStep {
    pub id: String,
    pub name: String,
    /// RFC 3339 timestamps as BuildKit reports them.
    pub started: Option<String>,
    pub completed: Option<String>,
    pub cached: bool,
    pub error: Option<String>,
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
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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

/// A caller-owned request to stop, never proof that execution stopped. Shared by
/// synchronous Build execution and subsequent async command phases.
///
/// [`Self::cancel`] takes the lock a Build step's process spawns under, so a step either
/// spawned before the cancel, and its wait sees the cancel and kills it, or never spawns.
#[derive(Clone, Debug, Default)]
pub struct Cancellation {
    token: tokio_util::sync::CancellationToken,
    spawning: std::sync::Arc<std::sync::Mutex<()>>,
}

impl Cancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        let _spawning = self.spawning.lock().expect("spawn lock");
        self.token.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn cancelled(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    /// Run `spawn` unless cancelled, under the lock [`Self::cancel`] takes: `None` once
    /// cancelled.
    pub(crate) fn spawn<T>(&self, spawn: impl FnOnce() -> T) -> Option<T> {
        let _spawning = self.spawning.lock().expect("spawn lock");
        (!self.token.is_cancelled()).then(spawn)
    }
}

/// A cancellation `token`'s holder drives, for a Build on its own computer. Cancelling the
/// token itself skips the spawn lock, which only a Machine's own Builds need.
impl From<tokio_util::sync::CancellationToken> for Cancellation {
    fn from(token: tokio_util::sync::CancellationToken) -> Self {
        Self {
            token,
            spawning: std::sync::Arc::default(),
        }
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
    /// Waiting budget; execution starts a separate clock after admission. `None`
    /// waits until admission, cancellation, or disconnect: capacity bounds the queue.
    pub queue_timeout: Option<Duration>,
    /// Fences BuildKit's own traffic before every solve. `None` leaves it
    /// unfenced, as on a developer's own computer.
    pub fence: Option<crate::FenceInstaller>,
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
            queue_capacity: 100,
            queue_timeout: None,
            fence: None,
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
        let seconds = |name: &str| {
            get(name)
                .map(|raw| {
                    raw.parse()
                        .map(Duration::from_secs)
                        .map_err(|_| BuildError::Prerequisite(format!("invalid {name}")))
                })
                .transpose()
        };
        if let Some(timeout) = seconds("PLOYZ_BUILD_QUEUE_TIMEOUT_SECONDS")? {
            policy.queue_timeout = Some(timeout);
        }
        if let Some(timeout) = seconds("PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS")? {
            policy.active_timeout = timeout;
        }
        policy.validate()?;
        Ok(policy)
    }

    /// Whether every process the last Build in `slot` started is gone, asked by the process
    /// that ran it: the slot recovers, its Build here known to have returned.
    ///
    /// # Errors
    /// The slot stays quarantined: a process of its last Build may still run, or Docker
    /// didn't confirm its builder removed. [`BuildError::Busy`] when another admission holds
    /// the slot, which says nothing: it may be recovering the slot and fail to.
    pub fn confirm_ended(&self, slot: usize) -> Result<(), BuildError> {
        Lock::try_acquire_own_slot(&self.state_directory, slot, &self.docker).map(drop)
    }

    /// Validate before allocating queue state or computing deadlines.
    /// # Errors
    /// Capacity is 0–1024; timeouts must be positive and at most 24 hours.
    pub fn validate(&self) -> Result<(), BuildError> {
        if self.queue_capacity > 1024
            || self
                .queue_timeout
                .iter()
                .chain([&self.active_timeout])
                .any(|timeout| timeout.is_zero() || *timeout > crate::TIMING.max_policy_timeout)
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
    pub(crate) fence: Option<crate::FenceInstaller>,
    pub(crate) access: crate::Access,
    pub(crate) reuse: crate::builder::Reuse,
    /// This attempt's own directory in its slot's upload staging, never another's.
    attempt: String,
    pub(crate) staging: crate::SealedStaging,
}
impl Admission {
    /// Recover abandoned build slots once after daemon restart. A slot whose
    /// last Build may still run stays quarantined; no Build is replayed.
    /// # Errors
    /// Busy slots, and quarantined slots with a surviving process or a builder
    /// Docker cannot confirm removed, remain unavailable.
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

    /// Acquire the first build slot under host policy, including the deadline
    /// that starts before upload.
    /// # Errors
    /// Refuses busy or quarantined state and reports filesystem failures.
    pub fn try_acquire_with(policy: &HostPolicy) -> Result<Self, BuildError> {
        Self::try_acquire_within(policy, BuildConcurrency::ONE)
    }

    /// Acquire any free, unquarantined slot among the first `concurrency`
    /// build slots. Each slot has its own builder, cache, and upload staging,
    /// so concurrent attempts never touch each other's state.
    /// # Errors
    /// Refuses when every slot is busy or quarantined (quarantine wins, so
    /// uncertainty is reported), and reports filesystem failures.
    pub fn try_acquire_within(
        policy: &HostPolicy,
        concurrency: BuildConcurrency,
    ) -> Result<Self, BuildError> {
        let mut quarantined = None;
        for slot in 0..usize::from(concurrency) {
            match Self::try_acquire_slot(policy, slot) {
                Ok(admission) => return Ok(admission),
                Err(BuildError::Busy) => {}
                Err(error) if error.is_unknown() => {
                    quarantined.get_or_insert(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(quarantined.unwrap_or(BuildError::Busy))
    }

    pub(crate) fn try_acquire_slot(policy: &HostPolicy, slot: usize) -> Result<Self, BuildError> {
        policy.validate()?;
        let resources = crate::policy::Resources::load(&policy.configuration_file)?;
        let lock = Lock::try_acquire_slot(&policy.state_directory, slot, &policy.docker)?;
        Ok(Self::owning(
            lock,
            resources,
            Deadline::starting_now(policy.active_timeout),
            Cancellation::default(),
            policy.fence,
            crate::builder::Reuse::WhenClean,
        ))
    }
    pub(crate) fn wait(docker: &Path, cancellation: &Cancellation) -> Result<Self, BuildError> {
        let lock = Lock::acquire(docker, cancellation)?;
        let resources = crate::policy::Resources::load(&HostPolicy::default().configuration_file)?;
        Ok(Self::owning(
            lock,
            resources,
            Deadline::starting_now(EXECUTION_TIMEOUT),
            cancellation.clone(),
            None,
            crate::builder::Reuse::Never,
        ))
    }
    fn owning(
        lock: Lock,
        resources: crate::policy::Resources,
        deadline: Deadline,
        cancellation: Cancellation,
        fence: Option<crate::FenceInstaller>,
        reuse: crate::builder::Reuse,
    ) -> Self {
        let attempt = uuid::Uuid::new_v4().to_string();
        let staging = crate::SealedStaging::under(&lock.upload_directory().join(&attempt));
        Self {
            lock,
            deadline,
            cancellation,
            resources,
            fence,
            access: crate::Access::Full,
            reuse,
            attempt,
            staging,
        }
    }
    /// Run this attempt with `access`, before its upload is admitted.
    #[must_use]
    pub fn with_access(mut self, access: crate::Access) -> Self {
        self.access = access;
        self
    }
    /// This attempt's capture: its own directory in its build slot's upload staging.
    pub(crate) fn upload_root(&self) -> PathBuf {
        self.lock.upload_directory().join(&self.attempt)
    }
    /// Where this attempt stages the Sealed values it opens, so its owner can take them back
    /// while the attempt still runs. No other attempt stages there.
    #[must_use]
    pub fn sealed_staging(&self) -> crate::SealedStaging {
        self.staging.clone()
    }
    /// The build slot this attempt owns, which [`HostPolicy::confirm_ended`] looks at again.
    #[must_use]
    pub fn slot(&self) -> usize {
        self.lock.slot()
    }
    /// A shared handle to this attempt's [`Cancellation`]; cancelling it makes [`Self::check`]
    /// fail with [`BuildError::Cancelled`].
    #[must_use]
    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }
    /// Time left before this attempt times out, counted from admission; zero once
    /// [`Self::check`] would fail with [`BuildError::TimedOut`].
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
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn restart_removes_abandoned_uploads_but_keeps_unknown_ownership_unavailable() {
        let root =
            std::env::temp_dir().join(format!("ployz-build-restart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
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
        // Startup removes a clean slot's workers; only the quarantined one below must stay.
        std::fs::remove_file(root.join("cleaned")).unwrap();
        let admission = Admission::try_acquire_with(&policy).unwrap();
        let upload = admission.upload().unwrap();
        assert!(!root.join("build-upload/abandoned").exists());
        assert!(matches!(
            Admission::try_acquire_with(&policy),
            Err(BuildError::Busy)
        ));
        drop(upload);
        // This test process owns the mark, so its Build may still be running.
        let mut lock = Lock::try_acquire_in(&root).unwrap();
        lock.quarantine().unwrap();
        drop(lock);
        std::fs::create_dir(root.join("build-upload")).unwrap();
        std::fs::write(root.join("build-upload/uncertain"), "still in use").unwrap();
        let refused = Admission::cleanup_abandoned(&policy).unwrap_err();
        assert!(refused.is_unknown());
        assert!(
            refused.to_string().contains(&format!(
                "process {} that owned the Build",
                std::process::id()
            )),
            "{refused}"
        );
        assert!(
            !root.join("cleaned").exists(),
            "cleanup touched the builder of a Build that may still run"
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

        // The same mark from before a reboot: none of its processes survives.
        let marker = root.join(format!("{}.lock", crate::builder_name()));
        let mark = std::fs::read_to_string(&marker).unwrap();
        let boot = mark.lines().find(|line| line.starts_with("boot ")).unwrap();
        std::fs::write(&marker, mark.replace(boot, "boot an-earlier-boot")).unwrap();
        Admission::cleanup_abandoned(&policy).unwrap();
        assert!(
            root.join("cleaned").exists(),
            "recovery did not remove the builder"
        );
        assert!(
            !root.join("build-upload").exists(),
            "recovery kept the abandoned inputs"
        );
        assert!(std::fs::read_to_string(&marker).unwrap().is_empty());
        drop(Admission::try_acquire_with(&policy).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_admissions_own_separate_slots_and_quarantine_stays_per_slot() {
        let root = std::env::temp_dir().join(format!("ployz-build-slots-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let policy = HostPolicy {
            state_directory: root.clone(),
            docker: root.join("docker"),
            ..Default::default()
        };
        crate::tests::executable(&policy.docker, "#!/bin/sh\nexit 0\n");
        let two = BuildConcurrency::parse("2").unwrap();
        let first = Admission::try_acquire_within(&policy, two).unwrap();
        let second = Admission::try_acquire_within(&policy, two).unwrap();
        assert_ne!(first.lock.name(), second.lock.name());
        assert_ne!(
            first.lock.upload_directory(),
            second.lock.upload_directory()
        );
        assert!(matches!(
            Admission::try_acquire_within(&policy, two),
            Err(BuildError::Busy)
        ));
        let first = first.upload().unwrap();
        let second = second.upload().unwrap();
        drop(first);
        drop(second);

        let mut lock = Lock::try_acquire_in(&root).unwrap();
        lock.quarantine().unwrap();
        drop(lock);
        assert!(
            Admission::try_acquire_with(&policy)
                .err()
                .unwrap()
                .is_unknown()
        );
        // Another slot has its own builder and staging, so it stays usable.
        let other = Admission::try_acquire_within(&policy, two).unwrap();
        assert!(
            Admission::try_acquire_within(&policy, two)
                .err()
                .unwrap()
                .is_unknown(),
            "a full Machine with a quarantined slot must report the uncertainty"
        );
        drop(other);
        std::fs::create_dir_all(root.join("build-upload-1/abandoned")).unwrap();
        assert!(
            Admission::cleanup_abandoned(&policy)
                .unwrap_err()
                .is_unknown()
        );
        assert!(
            !root.join("build-upload-1").exists(),
            "startup skipped a later slot's abandoned upload"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_build_slot_another_admission_holds_is_never_confirmed_ended() {
        let root = std::env::temp_dir().join(format!("ployz-build-busy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let policy = HostPolicy {
            state_directory: root.clone(),
            docker: root.join("docker"),
            ..Default::default()
        };
        // The Build that last used the slot never confirmed its end, and another admission
        // holds the slot to recover it, which it may fail to.
        let mut lock = Lock::try_acquire_in(&root).unwrap();
        lock.quarantine().unwrap();
        assert!(matches!(policy.confirm_ended(0), Err(BuildError::Busy)));
        drop(lock);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expired_admission_and_docker_report_configured_budget() {
        let root = std::env::temp_dir().join(format!("ployz-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
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
            recorder: None,
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
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
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

    /// A policy over a fresh private state directory, and the opened value a Build stages.
    fn staging_policy() -> (HostPolicy, ployz_core::OpenedSealed) {
        let root = std::env::temp_dir().join(format!("ployz-staging-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let opened = ployz_core::OpenedSealed::new(
            [("NPM_TOKEN".to_owned(), "opened-token".to_owned())].into(),
            Vec::new(),
        );
        (
            HostPolicy {
                state_directory: root,
                ..Default::default()
            },
            opened,
        )
    }

    /// `admission`'s upload, with the private directory a capture brings, and its root.
    fn uploaded(admission: Admission) -> (crate::remote::AdmittedUpload, PathBuf) {
        let root = admission.upload_root();
        let upload = admission.upload().unwrap();
        std::fs::create_dir(root.join("private")).unwrap();
        (upload, root)
    }

    /// Two Builds take the same slot in turn, and the first one's owner takes back its
    /// values only once the second staged its own: the second's values stay.
    #[test]
    fn a_late_revoke_takes_back_only_its_own_builds_values() {
        let (policy, opened) = staging_policy();
        let first = Admission::try_acquire_with(&policy).unwrap();
        let late = first.sealed_staging();
        drop(first);
        let second = Admission::try_acquire_with(&policy).unwrap();
        let staging = second.sealed_staging();
        let (_upload, root) = uploaded(second);
        let _staged = crate::Staged::stage(&staging, &opened, &[]).unwrap();

        late.revoke().unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join(crate::sealed_file("NPM_TOKEN"))).unwrap(),
            "opened-token",
            "an earlier Build's revoke removed the next Build's value"
        );
        std::fs::remove_dir_all(&policy.state_directory).unwrap();
    }

    /// An owner takes back its Build's values before the Build staged them: the Build stages
    /// none.
    #[test]
    fn a_revoke_before_staging_leaves_nothing_to_stage() {
        let (policy, opened) = staging_policy();
        let admission = Admission::try_acquire_with(&policy).unwrap();
        let staging = admission.sealed_staging();
        let (_upload, root) = uploaded(admission);

        staging.revoke().unwrap();

        assert!(matches!(
            crate::Staged::stage(&staging, &opened, &[]),
            Err(BuildError::Cancelled)
        ));
        assert!(!root.join(crate::sealed_file("NPM_TOKEN")).exists());
        std::fs::remove_dir_all(&policy.state_directory).unwrap();
    }

    #[test]
    fn machine_policy_defaults_and_invalid_arguments_are_bounded() {
        let policy = HostPolicy::from_settings(|_| None).unwrap();
        assert_eq!(policy.queue_capacity, 100);
        assert_eq!(policy.queue_timeout, None);
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
        assert_eq!(policy.queue_timeout, Some(Duration::from_secs(12)));
        assert_eq!(policy.active_timeout, Duration::from_secs(34));
    }
}
