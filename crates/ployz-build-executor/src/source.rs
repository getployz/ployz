use ployz_core::build::{
    BuildContextPath, BuildSource, GitSource, VerifiedBuildSource, VerifiedGitCommit,
};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

mod local_snapshot;
pub use local_snapshot::LocalSnapshotError;
use local_snapshot::consume_local_snapshot;
pub(super) use local_snapshot::stage_local_snapshot;

const GIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_GIT_ERROR_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(super) struct PreparedBuildSource {
    pub(super) context: PathBuf,
    pub(super) verified: VerifiedBuildSource,
}

impl PreparedBuildSource {
    #[must_use]
    pub fn context(&self) -> &Path {
        &self.context
    }

    #[must_use]
    pub const fn verified(&self) -> &VerifiedBuildSource {
        &self.verified
    }

    pub fn resolve_context_path(
        &self,
        path: &BuildContextPath,
    ) -> Result<PathBuf, GitCheckoutError> {
        canonical_descendant(&self.context, &self.context.join(path.as_str()))
    }
}

pub(super) async fn prepare_build_source(
    source: &BuildSource,
    workspace_root: &Path,
    workspace: &Path,
) -> Result<PreparedBuildSource, SourcePreparationError> {
    match source {
        BuildSource::Git { git } => checkout_git_source(git, workspace)
            .await
            .map_err(SourcePreparationError::Git),
        BuildSource::LocalSnapshot { digest, subdir } => {
            consume_local_snapshot(workspace_root, workspace, digest, subdir.as_ref())
                .await
                .map_err(SourcePreparationError::Local)
        }
    }
}

pub(super) async fn checkout_git_source(
    source: &GitSource,
    workspace: &Path,
) -> Result<PreparedBuildSource, GitCheckoutError> {
    ensure_private_directory(workspace).await?;
    let root = workspace.join("source");
    if tokio::fs::try_exists(&root)
        .await
        .map_err(|error| io_error("inspect source checkout", error))?
    {
        tokio::fs::remove_dir_all(&root)
            .await
            .map_err(|error| io_error("remove prior source checkout", error))?;
    }
    tokio::fs::create_dir(&root)
        .await
        .map_err(|error| io_error("create source checkout", error))?;
    set_private_directory(&root).await?;

    let askpass = workspace.join("git-askpass");
    write_askpass(&askpass).await?;
    let credential = source.credential();
    let credential_env = [
        ("GIT_ASKPASS", askpass.as_os_str()),
        ("GIT_TERMINAL_PROMPT", OsStr::new("0")),
        (
            "PLOYZ_GIT_USERNAME",
            OsStr::new(credential.username().as_str()),
        ),
        ("PLOYZ_GIT_SECRET", OsStr::new(credential.secret().secret())),
    ];

    run_git(
        source,
        &credential_env,
        ["init", "--quiet", root.to_string_lossy().as_ref()],
    )
    .await?;
    run_git(
        source,
        &credential_env,
        [
            "-C",
            root.to_string_lossy().as_ref(),
            "remote",
            "add",
            "origin",
            source.url().as_str(),
        ],
    )
    .await?;
    run_git(
        source,
        &credential_env,
        [
            "-C",
            root.to_string_lossy().as_ref(),
            "fetch",
            "--quiet",
            "--depth=1",
            "--no-tags",
            "origin",
            source.commit().as_str(),
        ],
    )
    .await?;
    run_git(
        source,
        &credential_env,
        [
            "-C",
            root.to_string_lossy().as_ref(),
            "checkout",
            "--quiet",
            "--detach",
            "FETCH_HEAD",
        ],
    )
    .await?;

    let head = git_stdout(
        source,
        &credential_env,
        [
            "-C",
            root.to_string_lossy().as_ref(),
            "rev-parse",
            "--verify",
            "HEAD",
        ],
    )
    .await?;
    let head = head.trim();
    if head != source.commit().as_str() {
        return Err(GitCheckoutError::CommitMismatch {
            expected: source.commit().as_str().to_owned(),
            actual: bounded(head),
        });
    }

    tokio::fs::remove_dir_all(root.join(".git"))
        .await
        .map_err(|error| io_error("remove git metadata", error))?;
    let _ = tokio::fs::remove_file(&askpass).await;
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|error| io_error("canonicalize source checkout", error))?;
    let context = match source.subdir() {
        Some(subdir) => canonical_descendant(&root, &root.join(subdir.as_str()))?,
        None => root.clone(),
    };
    Ok(PreparedBuildSource {
        context,
        verified: VerifiedBuildSource::Git {
            git: VerifiedGitCommit::from_source(source),
        },
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SourcePreparationError {
    #[error(transparent)]
    Git(#[from] GitCheckoutError),
    #[error(transparent)]
    Local(#[from] local_snapshot::LocalSnapshotPreparationError),
}

async fn run_git<const N: usize>(
    source: &GitSource,
    environment: &[(&str, &OsStr)],
    arguments: [&str; N],
) -> Result<(), GitCheckoutError> {
    let output = git_output(source, environment, arguments).await?;
    if output.status.success() {
        return Ok(());
    }
    Err(GitCheckoutError::GitFailed {
        message: redact_output(source, &output.stderr),
    })
}

async fn git_stdout<const N: usize>(
    source: &GitSource,
    environment: &[(&str, &OsStr)],
    arguments: [&str; N],
) -> Result<String, GitCheckoutError> {
    let output = git_output(source, environment, arguments).await?;
    if !output.status.success() {
        return Err(GitCheckoutError::GitFailed {
            message: redact_output(source, &output.stderr),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn git_output<const N: usize>(
    source: &GitSource,
    environment: &[(&str, &OsStr)],
    arguments: [&str; N],
) -> Result<std::process::Output, GitCheckoutError> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/nonexistent")
        .envs(environment.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GIT_TIMEOUT, command.output())
        .await
        .map_err(|_| GitCheckoutError::TimedOut)?
        .map_err(|error| io_error("run git", error))?;
    if output.stdout.len() > MAX_GIT_ERROR_BYTES || output.stderr.len() > MAX_GIT_ERROR_BYTES {
        return Err(GitCheckoutError::OutputTooLarge);
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if combined.contains(source.credential().secret().secret()) {
        return Err(GitCheckoutError::CredentialDisclosure);
    }
    Ok(output)
}

fn redact_output(source: &GitSource, bytes: &[u8]) -> String {
    bounded(
        &source
            .credential()
            .redact_secret_in(String::from_utf8_lossy(bytes).into_owned()),
    )
}

fn bounded(value: &str) -> String {
    value.chars().take(MAX_GIT_ERROR_BYTES).collect()
}

fn canonical_descendant(root: &Path, candidate: &Path) -> Result<PathBuf, GitCheckoutError> {
    let root = std::fs::canonicalize(root).map_err(|error| io_error("canonicalize root", error))?;
    let candidate = std::fs::canonicalize(candidate)
        .map_err(|error| io_error("canonicalize build path", error))?;
    if !candidate.starts_with(&root) {
        return Err(GitCheckoutError::PathEscapesCheckout { candidate });
    }
    Ok(candidate)
}

async fn ensure_private_directory(path: &Path) -> Result<(), GitCheckoutError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|error| io_error("create build workspace", error))?;
    set_private_directory(path).await
}

#[cfg(unix)]
async fn set_private_directory(path: &Path) -> Result<(), GitCheckoutError> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|error| io_error("set build workspace permissions", error))
}

#[cfg(not(unix))]
async fn set_private_directory(_path: &Path) -> Result<(), GitCheckoutError> {
    Ok(())
}

async fn write_askpass(path: &Path) -> Result<(), GitCheckoutError> {
    tokio::fs::write(
        path,
        b"#!/bin/sh\ncase \"$1\" in\n  *Username*) printf '%s\\n' \"$PLOYZ_GIT_USERNAME\" ;;\n  *) printf '%s\\n' \"$PLOYZ_GIT_SECRET\" ;;\nesac\n",
    )
    .await
    .map_err(|error| io_error("write git credential helper", error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| io_error("set git credential helper permissions", error))?;
    }
    Ok(())
}

fn io_error(action: &'static str, error: std::io::Error) -> GitCheckoutError {
    GitCheckoutError::Io {
        action,
        message: error.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GitCheckoutError {
    #[error("git checkout timed out")]
    TimedOut,
    #[error("git checkout output exceeded its bound")]
    OutputTooLarge,
    #[error("git emitted the source credential")]
    CredentialDisclosure,
    #[error("git checkout failed: {message}")]
    GitFailed { message: String },
    #[error("git returned commit {actual}, expected {expected}")]
    CommitMismatch { expected: String, actual: String },
    #[error("build path escapes the checked-out source: {candidate:?}")]
    PathEscapesCheckout { candidate: PathBuf },
    #[error("failed to {action}: {message}")]
    Io {
        action: &'static str,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn source(context: &Path) -> PreparedBuildSource {
        PreparedBuildSource {
            context: context.to_path_buf(),
            verified: VerifiedBuildSource::Git {
                git: VerifiedGitCommit::from_source(
                    &GitSource::try_new(
                        "https://example.test/repo.git",
                        "0123456789abcdef0123456789abcdef01234567",
                        "git",
                        "secret",
                        None::<String>,
                    )
                    .expect("source"),
                ),
            },
        }
    }

    #[test]
    fn checked_out_source_resolves_only_canonical_descendants() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("source");
        let context = root.join("app");
        fs::create_dir_all(&context).expect("context");
        fs::write(context.join("Dockerfile"), "FROM scratch\n").expect("dockerfile");
        let source = source(&context);
        let dockerfile = BuildContextPath::try_new("Dockerfile").expect("path");
        assert_eq!(
            source
                .resolve_context_path(&dockerfile)
                .expect("in-tree file"),
            fs::canonicalize(context.join("Dockerfile")).expect("canonical file")
        );
    }

    #[cfg(unix)]
    #[test]
    fn checked_out_source_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("source");
        let context = root.join("app");
        fs::create_dir_all(&context).expect("context");
        let outside = temp.path().join("outside-Dockerfile");
        fs::write(&outside, "FROM scratch\n").expect("outside file");
        symlink(&outside, context.join("Dockerfile")).expect("symlink");
        let source = source(&context);
        let dockerfile = BuildContextPath::try_new("Dockerfile").expect("path");
        assert!(matches!(
            source.resolve_context_path(&dockerfile),
            Err(GitCheckoutError::PathEscapesCheckout { .. })
        ));
    }

    #[tokio::test]
    async fn askpass_script_contains_no_secret_and_is_private() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("askpass");
        write_askpass(&path).await.expect("askpass");
        let body = fs::read_to_string(&path).expect("body");
        assert!(body.contains("PLOYZ_GIT_SECRET"));
        assert!(!body.contains("super-secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
