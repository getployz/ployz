//! The pinned BuildKit builder: replaced for each attempt, removed afterwards,
//! and leaving its dedicated cache volume behind for the next one.

use std::{
    fs,
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::PathBuf,
};

use crate::{BUILDKIT_IMAGE, BuildError, Docker, Streams, builder_name};

/// Longest a command waits for another local build to release the builder.
const QUEUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Exclusive use of the Ployz builder and its retained cache for one attempt.
pub(crate) struct Builder<'a> {
    docker: &'a Docker<'a>,
    name: String,
    lock: Option<Lock>,
}

impl<'a> Builder<'a> {
    /// Provision a container for this attempt, keeping the retained cache.
    ///
    /// The container is always replaced rather than reused: an earlier
    /// attempt that never confirmed its end may have left work running in it,
    /// and its cache volume survives the replacement either way.
    ///
    /// # Errors
    /// Fails when Buildx is unavailable or the container cannot be created.
    pub(crate) fn acquire(
        docker: &'a Docker<'a>,
        lock: Lock,
        resources: &crate::policy::Resources,
    ) -> Result<Self, BuildError> {
        let name = builder_name();
        let mut builder = Self {
            docker,
            name,
            lock: Some(lock),
        };
        // Mark before any Docker mutation so a killed daemon cannot silently
        // reuse state whose termination was never observed.
        builder.lock.as_mut().expect("owned lock").quarantine()?;
        let result = remove(&docker.releasing(), &builder.name)
            .and_then(|()| create(docker, &builder.name, resources));
        if let Err(error) = result {
            return builder.finish(Err(error));
        }
        Ok(builder)
    }

    /// Read the running worker, not the Machine's advertised architecture.
    pub(crate) fn native_platform(
        &self,
        targets: &[crate::Target],
        resources: &crate::policy::Resources,
    ) -> Result<String, BuildError> {
        let info = self.docker.run(
            "inspect the image store",
            &["info", "--format", "{{json .}}"],
            Streams::Captured,
        )?;
        let info: serde_json::Value = serde_json::from_str(&info).map_err(|_| {
            BuildError::Prerequisite("Docker reported invalid image-store capability".into())
        })?;
        resources.check_support(&info)?;
        if !info
            .get("DriverStatus")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row == &serde_json::json!(["driver-type", "io.containerd.snapshotter.v1"])
                })
            })
        {
            return Err(BuildError::Prerequisite(
                "Ployz Builds require Docker's containerd image store".into(),
            ));
        }
        let architecture = match info.get("Architecture").and_then(serde_json::Value::as_str) {
            Some("x86_64" | "amd64") => "amd64",
            Some("aarch64" | "arm64") => "arm64",
            _ => {
                return Err(BuildError::Prerequisite(
                    "the build host must support Linux AMD64 or ARM64".into(),
                ));
            }
        };
        if info.get("OSType").and_then(serde_json::Value::as_str) != Some("linux") {
            return Err(BuildError::Prerequisite(
                "the build host must run Linux containers".into(),
            ));
        }
        let native = format!("linux/{architecture}");
        self.docker.run(
            "start the BuildKit worker",
            &["buildx", "inspect", &self.name, "--bootstrap"],
            Streams::Captured,
        )?;
        let workers = self.docker.run(
            "inspect BuildKit platforms",
            &["buildx", "ls", "--format", "{{json .}}"],
            Streams::Captured,
        )?;
        let builder = workers
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|builder| {
                builder.get("Name").and_then(serde_json::Value::as_str) == Some(&self.name)
            });
        let nodes = builder
            .as_ref()
            .and_then(|builder| builder.get("Nodes"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                BuildError::Prerequisite(
                    "the selected BuildKit worker reported no capability".into(),
                )
            })?;
        for target in targets {
            let requested = target.platform.as_deref().unwrap_or(&native);
            if !nodes.iter().any(|node| {
                node.get("Status").and_then(serde_json::Value::as_str) == Some("running")
                    && node
                        .get("Platforms")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|platforms| {
                            platforms
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .any(|platform| crate::covers(platform, requested))
                        })
            }) {
                return Err(BuildError::Prerequisite(format!(
                    "the running BuildKit worker cannot build {requested}"
                )));
            }
        }
        Ok(native)
    }

    pub(crate) fn run(
        &self,
        arguments: &[String],
        started: impl FnOnce(),
    ) -> Result<(), BuildError> {
        let borrowed = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        self.docker
            .run_started("the build", &borrowed, Streams::Inherited, started)
            .map(|_| ())
    }

    /// Confirm teardown before releasing retained state; failure preserves its
    /// quarantine across daemon restart as well as competing direct calls.
    pub(crate) fn finish<T>(mut self, result: Result<T, BuildError>) -> Result<T, BuildError> {
        let cleanup = remove(&self.docker.releasing(), &self.name);
        let stage = result
            .as_ref()
            .err()
            .map_or(crate::Stage::Cleanup, BuildError::stage);
        let cleared = if cleanup.is_ok() && !result.as_ref().is_err_and(|error| error.is_unknown())
        {
            self.lock.as_mut().expect("owned lock").clear()
        } else {
            Ok(())
        };
        self.lock.take();
        if let Err(error) = cleared {
            return Err(match result {
                Ok(_) => error.at(stage),
                Err(cause) => {
                    BuildError::Result(format!("{cause}; cleanup failed: {error}")).at(stage)
                }
            });
        }
        match cleanup {
            Ok(()) => result,
            Err(error) => Err(BuildError::UncertainTermination(match result {
                Ok(_) => format!("output handling completed; cleanup failed: {error}"),
                Err(cause) => format!("{cause}; cleanup failed: {error}"),
            })
            .at(stage)),
        }
    }
}

impl Drop for Builder<'_> {
    fn drop(&mut self) {
        // Panic/unwind still attempts cleanup. The marker was persisted before
        // execution, so a failed cleanup cannot unlock unsafe retained state.
        if let Some(lock) = self.lock.as_mut()
            && remove(&self.docker.releasing(), &self.name).is_ok()
        {
            let _ = lock.clear();
        }
    }
}

fn create(
    docker: &Docker<'_>,
    name: &str,
    resources: &crate::policy::Resources,
) -> Result<(), BuildError> {
    // Always supply a config, so ambient Buildx configuration cannot replace
    // execution-host policy. Empty config retains the pinned BuildKit defaults.
    let config = docker.working_dir.join("buildkitd.toml");
    fs::write(&config, resources.buildkit_config()).map_err(|error| {
        BuildError::Prerequisite(format!("write BuildKit host configuration: {error}"))
    })?;
    let mut arguments = vec![
        "buildx".into(),
        "create".into(),
        "--name".into(),
        name.into(),
        "--driver".into(),
        "docker-container".into(),
        "--driver-opt".into(),
        format!("image={BUILDKIT_IMAGE}"),
        "--driver-opt".into(),
        "network=host".into(),
        "--buildkitd-config".into(),
        config.to_string_lossy().into_owned(),
    ];
    arguments.extend(resources.worker_arguments());
    docker
        .run(
            "create the build container",
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
            Streams::Captured,
        )
        .map(|_| ())
}

fn remove(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    match docker.run(
        "remove the build container",
        &["buildx", "rm", "--keep-state", name],
        Streams::Captured,
    ) {
        Ok(_) => Ok(()),
        // Already gone is the state this asks for.
        Err(BuildError::Docker { diagnostic, .. }) if diagnostic.contains("no builder") => Ok(()),
        Err(error) => Err(error),
    }
}

/// Serializes local attempts sharing one builder and its retained cache.
///
/// Held for the whole attempt. Uncertain termination quarantines the retained state.
#[derive(Clone)]
pub(crate) struct Lock {
    file: std::sync::Arc<LockedFile>,
    pub(crate) directory: PathBuf,
}

struct LockedFile(fs::File);

impl Lock {
    /// Wait for exclusive use of this user's builder.
    ///
    /// # Errors
    /// Fails when the lock file cannot be opened or locked.
    pub(crate) fn acquire() -> Result<Self, BuildError> {
        Self::acquire_in(&directory())
    }

    fn acquire_in(directory: &std::path::Path) -> Result<Self, BuildError> {
        let deadline = std::time::Instant::now() + QUEUE_TIMEOUT;
        loop {
            match Self::try_acquire_in(directory) {
                Err(BuildError::Busy) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                result => return result,
            }
        }
    }

    pub(crate) fn try_acquire_in(directory: &std::path::Path) -> Result<Self, BuildError> {
        let lock = Self::open_locked(directory)?;
        if lock.file.0.metadata().map_err(lock_error)?.len() != 0 {
            return Err(lock.uncertain());
        }
        Ok(lock)
    }

    fn uncertain(&self) -> BuildError {
        BuildError::UncertainTermination(format!(
            "retained builder state is quarantined; confirm builder {} and its host processes have stopped before clearing {}",
            builder_name(),
            self.directory
                .join(format!("{}.lock", builder_name()))
                .display()
        ))
    }

    /// One bounded teardown of an abandoned builder on daemon startup. The
    /// marker remains: removing a container cannot prove an orphaned host
    /// process stopped, so cleanup alone must never authorize conflicting work.
    pub(crate) fn cleanup_abandoned(policy: &crate::HostPolicy) -> Result<(), BuildError> {
        let lock = Self::open_locked(&policy.state_directory)?;
        if lock.file.0.metadata().map_err(lock_error)?.len() == 0 {
            return crate::upload::remove_abandoned(&lock.directory.join("build-upload")).map_err(
                |error| BuildError::Prerequisite(format!("remove abandoned Build upload: {error}")),
            );
        }
        let environment = crate::upload::environment(&lock.directory.join("build-upload"));
        let docker = Docker {
            program: &policy.docker,
            environment: &environment,
            working_dir: &lock.directory,
            deadline: crate::Deadline::starting_now(crate::CLEANUP_TIMEOUT),
            cancellation: None,
            progress: None,
        };
        if let Err(error) = remove(&docker, &builder_name()) {
            return Err(BuildError::UncertainTermination(format!(
                "{}; abandoned builder cleanup failed: {error}",
                lock.uncertain()
            )));
        }
        Err(lock.uncertain())
    }

    fn open_locked(directory: &std::path::Path) -> Result<Self, BuildError> {
        use rustix::fs::{FlockOperation, flock};
        use std::os::unix::fs::MetadataExt as _;
        match fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(lock_error(error)),
        }
        let metadata = fs::symlink_metadata(directory).map_err(lock_error)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o022 != 0
        {
            return Err(BuildError::Prerequisite(
                "builder state must be an owned directory without group/other write access".into(),
            ));
        }
        let path = directory.join(format!("{}.lock", builder_name()));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&path)
            .map_err(lock_error)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => return Err(BuildError::Busy),
            Err(error) => return Err(lock_error(error.into())),
        }
        Ok(Self {
            file: std::sync::Arc::new(LockedFile(file)),
            directory: directory.to_owned(),
        })
    }

    pub(crate) fn clear(&mut self) -> Result<(), BuildError> {
        self.file.0.set_len(0).map_err(lock_error)?;
        self.file.0.sync_all().map_err(lock_error)
    }

    /// Persist uncertainty across process exit before releasing the OS lock.
    pub(crate) fn quarantine(&mut self) -> Result<(), BuildError> {
        use std::io::Write as _;
        (&self.file.0)
            .write_all(b"termination unconfirmed\n")
            .map_err(lock_error)?;
        self.file.0.sync_all().map_err(lock_error)
    }
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        // Another thread may have forked a child that briefly inherited this
        // CLOEXEC descriptor. Release ownership now, without waiting for exec.
        let _ = rustix::fs::flock(&self.0, rustix::fs::FlockOperation::Unlock);
    }
}

/// Stable across HOME, captures, and Docker configuration directories: a local
/// CLI and daemon with the same builder name must lock the same retained state.
pub(crate) fn directory() -> PathBuf {
    PathBuf::from("/var/tmp").join(format!(
        "ployz-build-{}",
        rustix::process::getuid().as_raw()
    ))
}

fn lock_error(error: std::io::Error) -> BuildError {
    BuildError::Prerequisite(format!("access the build lock: {error}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn dockerfile_platforms_follow_the_running_workers_capabilities() {
        let directory =
            std::env::temp_dir().join(format!("ployz-platform-{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let program = directory.join("docker");
        crate::tests::executable(
            &program,
            &format!(
                r#"#!/bin/sh
case "$1 $2" in
  'info --format') echo '{{"DriverStatus":[["driver-type","io.containerd.snapshotter.v1"]],"Architecture":"amd64","OSType":"linux"}}' ;;
  'buildx ls') echo '{{"Name":"{}","Nodes":[{{"Status":"running","Platforms":["linux/amd64","linux/386","linux/arm/v7","linux/ppc64le"]}}]}}' ;;
esac
"#,
                builder_name()
            ),
        );
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: &directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
        };
        let resources = crate::policy::Resources::default();
        let builder = Builder::acquire(
            &docker,
            Lock::try_acquire_in(&directory).unwrap(),
            &resources,
        )
        .unwrap();
        for platform in ["linux/386", "linux/arm/v7", "linux/ppc64le", "linux/s390x"] {
            let result = builder.native_platform(
                &[crate::Target {
                    name: "api".into(),
                    platform: Some(platform.into()),
                }],
                &resources,
            );
            assert_eq!(
                result.is_ok(),
                platform != "linux/s390x",
                "{platform}: {result:?}"
            );
        }
        builder.finish(Ok(())).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn quarantine_clear_failure_preserves_cleanup_and_prior_failure_stages() {
        for prior_failure in [false, true] {
            let directory =
                std::env::temp_dir().join(format!("ployz-clear-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(
                &directory,
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .unwrap();
            let program = directory.join("docker");
            crate::tests::executable(&program, "#!/bin/sh\nexit 0\n");
            let environment = BTreeMap::new();
            let docker = Docker {
                program: &program,
                environment: &environment,
                working_dir: &directory,
                deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
                cancellation: None,
                progress: None,
            };
            let lock = Lock::try_acquire_in(&directory).unwrap();
            let mut builder =
                Builder::acquire(&docker, lock, &crate::policy::Resources::default()).unwrap();
            // A read-only descriptor deterministically makes marker truncation fail.
            builder.lock.as_mut().unwrap().file = std::sync::Arc::new(LockedFile(
                fs::File::open(directory.join(format!("{}.lock", builder_name()))).unwrap(),
            ));
            let result = if prior_failure {
                Err(BuildError::Result("earlier build failure".into()).at(crate::Stage::Building))
            } else {
                Ok(())
            };
            let error = builder.finish(result).unwrap_err();
            assert_eq!(
                error.stage(),
                if prior_failure {
                    crate::Stage::Building
                } else {
                    crate::Stage::Cleanup
                }
            );
            assert!(!error.is_unknown(), "Docker termination was confirmed");
            if prior_failure {
                assert!(error.to_string().contains("earlier build failure"));
            }
            assert!(error.to_string().contains("build lock"));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn admission_refuses_competitors_and_uncertain_builder_ownership() {
        let directory =
            std::env::temp_dir().join(format!("ployz-admission-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(
            &directory,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let lock = Lock::try_acquire_in(&directory).unwrap();
        assert!(matches!(
            Lock::try_acquire_in(&directory),
            Err(BuildError::Busy)
        ));
        drop(lock);
        let mut lock = Lock::try_acquire_in(&directory).unwrap();
        lock.quarantine().unwrap();
        drop(lock);
        assert!(matches!(
            Lock::try_acquire_in(&directory),
            Err(BuildError::UncertainTermination(_))
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn every_attempt_replaces_the_container_and_leaves_its_cache() {
        let directory = std::env::temp_dir().join(format!("ployz-builder-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(
            &directory,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let program = directory.join("docker");
        crate::tests::executable(
            &program,
            &format!(
                "#!/bin/sh\ncase \"$1\" in --ready) exit 0 ;; esac\nprintf '%s\\n' \"$*\" >> '{}/calls'\nexit 0\n",
                directory.display()
            ),
        );
        let environment = BTreeMap::new();
        let docker = Docker {
            program: &program,
            environment: &environment,
            working_dir: &directory,
            deadline: crate::Deadline::starting_now(crate::EXECUTION_TIMEOUT),
            cancellation: None,
            progress: None,
        };

        let lock = Lock::acquire_in(&directory).unwrap();
        let builder =
            Builder::acquire(&docker, lock, &crate::policy::Resources::default()).unwrap();
        let name = builder_name();
        let acquired = fs::read_to_string(directory.join("calls")).unwrap();
        // A container left by an earlier attempt is stale, so it is replaced.
        assert_eq!(
            acquired.lines().collect::<Vec<_>>(),
            [
                format!("buildx rm --keep-state {name}"),
                format!(
                    "buildx create --name {name} --driver docker-container --driver-opt image={BUILDKIT_IMAGE} --driver-opt network=host --buildkitd-config {}",
                    directory.join("buildkitd.toml").display()
                ),
            ]
        );

        drop(builder);
        let released = fs::read_to_string(directory.join("calls")).unwrap();
        // Teardown keeps the cache volume the container was built with.
        assert_eq!(
            released.lines().last(),
            Some(format!("buildx rm --keep-state {name}").as_str())
        );
        fs::remove_dir_all(&directory).unwrap();
    }
}
