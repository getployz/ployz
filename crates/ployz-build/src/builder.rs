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
    _lock: Lock,
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
    pub(crate) fn acquire(docker: &'a Docker<'a>, lock: Lock) -> Result<Self, BuildError> {
        let name = builder_name();
        // Best effort: any container left behind is stale by construction.
        let _ = remove(&docker.releasing(), &name);
        create(docker, &name)?;
        Ok(Self {
            docker,
            name,
            _lock: lock,
        })
    }

    /// Run one build, bounded by the attempt's deadline.
    ///
    /// # Errors
    /// Returns BuildKit's own diagnosis, a timeout once the builder is
    /// confirmed stopped, or an uncertain termination when it is not.
    pub(crate) fn run(&self, arguments: &[String]) -> Result<(), BuildError> {
        let borrowed = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        match self.docker.run("the build", &borrowed, Streams::Inherited) {
            Ok(_) => Ok(()),
            Err(BuildError::TimedOut(seconds)) => Err(self.terminate(seconds)),
            Err(error) => Err(error),
        }
    }

    /// The attempt was terminated; report whether its builder stopped too.
    fn terminate(&self, seconds: u64) -> BuildError {
        match remove(&self.docker.releasing(), &self.name) {
            Ok(()) => BuildError::TimedOut(seconds),
            Err(error) => BuildError::UncertainTermination(error.to_string()),
        }
    }
}

impl Drop for Builder<'_> {
    /// Remove the container, keeping the cache volume it was built with.
    ///
    /// A removal that fails is reported rather than discarded: the image this
    /// attempt built stays usable, and the next attempt replaces the container.
    fn drop(&mut self) {
        if let Err(error) = remove(&self.docker.releasing(), &self.name) {
            eprintln!(
                "WARNING: build container '{}' was left behind: {error}. The next build replaces it.",
                self.name
            );
        }
    }
}

fn create(docker: &Docker<'_>, name: &str) -> Result<(), BuildError> {
    let image = format!("image={BUILDKIT_IMAGE}");
    // Host networking keeps builds reaching the hosts they reached while
    // Docker built them with its own embedded BuildKit.
    docker
        .run(
            "create the build container",
            &[
                "buildx",
                "create",
                "--name",
                name,
                "--driver",
                "docker-container",
                "--driver-opt",
                &image,
                "--driver-opt",
                "network=host",
            ],
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
/// Held for the whole attempt and released when it ends, however it ends.
pub(crate) struct Lock {
    _file: fs::File,
}

impl Lock {
    /// Wait for exclusive use of this user's builder.
    ///
    /// # Errors
    /// Fails when the lock file cannot be opened or locked.
    pub(crate) fn acquire() -> Result<Self, BuildError> {
        Self::acquire_in(&directory())
    }

    fn acquire_in(directory: &std::path::Path) -> Result<Self, BuildError> {
        use rustix::fs::{FlockOperation, flock};

        let path = directory.join(format!("{}.lock", builder_name()));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|error| {
                BuildError::Prerequisite(format!("open the build lock {}: {error}", path.display()))
            })?;
        if flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok() {
            return Ok(Self { _file: file });
        }
        // Poll rather than block: a peer that never releases the builder must
        // not leave this command waiting forever.
        eprintln!("Waiting for another local Ployz build to finish.");
        let deadline = std::time::Instant::now() + QUEUE_TIMEOUT;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok() {
                return Ok(Self { _file: file });
            }
        }
        Err(BuildError::Prerequisite(format!(
            "another local Ployz build has held the builder for {}s; retry once it finishes",
            QUEUE_TIMEOUT.as_secs()
        )))
    }
}

/// This user's own Ployz directory, so a shared temporary directory cannot
/// hold the lock hostage. Falls back to the temporary directory without one.
fn directory() -> PathBuf {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return std::env::temp_dir();
    };
    let directory = home.join(".ployz");
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => directory,
        Err(_) => std::env::temp_dir(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn every_attempt_replaces_the_container_and_leaves_its_cache() {
        let directory = std::env::temp_dir().join(format!("ployz-builder-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
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
        };

        let lock = Lock::acquire_in(&directory).unwrap();
        let builder = Builder::acquire(&docker, lock).unwrap();
        let name = builder_name();
        let acquired = fs::read_to_string(directory.join("calls")).unwrap();
        // A container left by an earlier attempt is stale, so it is replaced.
        assert_eq!(
            acquired.lines().collect::<Vec<_>>(),
            [
                format!("buildx rm --keep-state {name}"),
                format!(
                    "buildx create --name {name} --driver docker-container --driver-opt image={BUILDKIT_IMAGE} --driver-opt network=host"
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
