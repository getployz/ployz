use std::fs::File;
use std::path::Path;
use std::time::Duration;

use crate::deploy::command::CurrentTreeDeployCommand;
use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::PloyzctlExecutionOutput;

use super::DeployExecutionError;

mod cloud;
mod standalone;

const CLOUD_URL_ENV: &str = "PLOYZ_CLOUD_URL";
pub(super) const OBSERVE_TIMEOUT: Duration = Duration::from_secs(40 * 60);

enum CurrentTreeTarget {
    Cloud { url: String },
    Standalone,
}

pub(crate) async fn execute(
    command: CurrentTreeDeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let cwd = std::env::current_dir().map_err(current_tree_error)?;
    let _working_tree_lock = CurrentTreeLock::acquire(&cwd)?;
    match target_from_environment() {
        CurrentTreeTarget::Cloud { url } => cloud::execute(command, config, &url).await,
        CurrentTreeTarget::Standalone => standalone::execute(command, config).await,
    }
}

#[derive(Debug)]
struct CurrentTreeLock {
    _file: File,
}

impl CurrentTreeLock {
    fn acquire(working_tree: &Path) -> Result<Self, PloyzctlExecutionError> {
        let state_directory = working_tree.join(".ployz");
        create_private_directory(&state_directory)?;
        let lock_path = state_directory.join("current-tree.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&lock_path).map_err(current_tree_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(current_tree_error)?;
        }
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => current_tree_error(format!(
                "another current-tree deploy is already running for {}",
                working_tree.display()
            )),
            std::fs::TryLockError::Error(error) => {
                current_tree_error(format!("acquire current-tree deploy lock: {error}"))
            }
        })?;
        Ok(Self { _file: file })
    }
}

fn create_private_directory(path: &Path) -> Result<(), PloyzctlExecutionError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(current_tree_error(error)),
    }
    let metadata = std::fs::symlink_metadata(path).map_err(current_tree_error)?;
    if !metadata.file_type().is_dir() {
        return Err(current_tree_error(format!(
            "current-tree state path is not a directory: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(current_tree_error)?;
    }
    Ok(())
}

fn target_from_environment() -> CurrentTreeTarget {
    match std::env::var(CLOUD_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(url) => CurrentTreeTarget::Cloud { url },
        None => CurrentTreeTarget::Standalone,
    }
}

fn write_private(path: &Path, contents: &str, mode: u32) -> Result<(), PloyzctlExecutionError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    use std::io::Write;
    let mut file = options.open(path).map_err(current_tree_error)?;
    file.write_all(contents.as_bytes())
        .map_err(current_tree_error)
}

fn current_tree_error(message: impl std::fmt::Display) -> PloyzctlExecutionError {
    DeployExecutionError::CurrentTree {
        message: message.to_string(),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_tree_lock_contends_then_releases() {
        let tree = tempfile::tempdir().expect("tree");
        let first = CurrentTreeLock::acquire(tree.path()).expect("first lock");

        let error = CurrentTreeLock::acquire(tree.path()).expect_err("same tree contends");
        assert!(error.to_string().contains("already running"));

        drop(first);
        CurrentTreeLock::acquire(tree.path()).expect("released lock can be reacquired");
    }

    #[test]
    fn current_tree_lock_is_not_global() {
        let first_tree = tempfile::tempdir().expect("first tree");
        let second_tree = tempfile::tempdir().expect("second tree");

        let _first = CurrentTreeLock::acquire(first_tree.path()).expect("first tree lock");
        let _second = CurrentTreeLock::acquire(second_tree.path()).expect("second tree lock");
    }

    #[cfg(unix)]
    #[test]
    fn current_tree_lock_material_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let tree = tempfile::tempdir().expect("tree");
        let _lock = CurrentTreeLock::acquire(tree.path()).expect("lock");

        assert_eq!(
            std::fs::metadata(tree.path().join(".ployz"))
                .expect("state directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(tree.path().join(".ployz/current-tree.lock"))
                .expect("lock file")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
