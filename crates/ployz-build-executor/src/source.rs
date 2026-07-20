use ployz_core::build::{
    BuildContextPath, BuildSource, GitSource, LocalSnapshotDigest, VerifiedBuildSource,
    VerifiedGitCommit,
};
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

mod local_snapshot;
use local_snapshot::consume_local_snapshot;
pub(super) use local_snapshot::stage_local_snapshot;

const GIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_GIT_ERROR_BYTES: usize = 64 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SNAPSHOT_ENTRIES: usize = 100_000;
const SNAPSHOT_DIGEST_VERSION: &[u8] = b"ployz.local-snapshot.v1";

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
            consume_local_snapshot(workspace_root, workspace, digest, subdir.as_ref()).await
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

fn stage_local_snapshot_blocking(
    source_root: &Path,
    prepared_root: &Path,
    subdir: Option<BuildContextPath>,
) -> Result<BuildSource, LocalSnapshotError> {
    let source_root = std::fs::canonicalize(source_root)
        .map_err(|error| snapshot_io("canonicalize local source", error))?;
    if !source_root.is_dir() {
        return Err(LocalSnapshotError::SourceNotDirectory);
    }
    std::fs::create_dir_all(prepared_root)
        .map_err(|error| snapshot_io("create prepared snapshot root", error))?;
    set_private_directory_sync(prepared_root)?;
    let staging = prepared_root.join("staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|error| snapshot_io("remove prior staged snapshot", error))?;
    }
    std::fs::create_dir(&staging).map_err(|error| snapshot_io("create staged snapshot", error))?;
    set_private_directory_sync(&staging)?;
    let result =
        copy_snapshot_tree(&source_root, &staging).and_then(|()| digest_snapshot(&staging));
    let digest = match result {
        Ok(digest) => digest,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let armed = prepared_root.join(digest_directory(&digest));
    if armed.exists() {
        std::fs::remove_dir_all(&armed)
            .map_err(|error| snapshot_io("replace prepared local snapshot", error))?;
    }
    std::fs::rename(&staging, &armed).map_err(|error| snapshot_io("arm local snapshot", error))?;
    Ok(BuildSource::LocalSnapshot { digest, subdir })
}

fn copy_snapshot_tree(
    source_root: &Path,
    destination_root: &Path,
) -> Result<(), LocalSnapshotError> {
    let mut entries = vec![PathBuf::new()];
    let mut copied_bytes = 0_u64;
    let mut entry_count = 0_usize;
    let mut index = 0;
    while index < entries.len() {
        let relative = entries
            .get(index)
            .cloned()
            .ok_or(LocalSnapshotError::EntryLimitExceeded)?;
        index += 1;
        let source = source_root.join(&relative);
        let mut children = std::fs::read_dir(&source)
            .map_err(|error| snapshot_io("read local source directory", error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| snapshot_io("read local source entry", error))?;
        children.sort_by(|left, right| {
            left.file_name()
                .as_encoded_bytes()
                .cmp(right.file_name().as_encoded_bytes())
        });
        for child in children {
            entry_count = entry_count.saturating_add(1);
            if entry_count > MAX_SNAPSHOT_ENTRIES {
                return Err(LocalSnapshotError::EntryLimitExceeded);
            }
            let name = child.file_name();
            let name_text = name.to_str().ok_or(LocalSnapshotError::NonPortablePath)?;
            if relative.as_os_str().is_empty() && matches!(name_text, ".git" | ".ployz") {
                continue;
            }
            let child_relative = relative.join(name_text);
            let source_path = child.path();
            let destination = destination_root.join(&child_relative);
            let before = std::fs::symlink_metadata(&source_path)
                .map_err(|error| snapshot_io("inspect local source entry", error))?;
            let file_type = before.file_type();
            if file_type.is_dir() {
                std::fs::create_dir(&destination)
                    .map_err(|error| snapshot_io("create snapshot directory", error))?;
                set_private_directory_sync(&destination)?;
                entries.push(child_relative);
            } else if file_type.is_file() {
                copied_bytes = copied_bytes
                    .checked_add(before.len())
                    .ok_or(LocalSnapshotError::ByteLimitExceeded)?;
                if copied_bytes > MAX_SNAPSHOT_BYTES {
                    return Err(LocalSnapshotError::ByteLimitExceeded);
                }
                std::fs::copy(&source_path, &destination)
                    .map_err(|error| snapshot_io("copy local source file", error))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = if before.permissions().mode() & 0o111 == 0 {
                        0o600
                    } else {
                        0o700
                    };
                    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode))
                        .map_err(|error| snapshot_io("set snapshot file permissions", error))?;
                }
                let after = std::fs::symlink_metadata(&source_path)
                    .map_err(|error| snapshot_io("reinspect local source file", error))?;
                if metadata_changed(&before, &after) {
                    return Err(LocalSnapshotError::SourceMutated {
                        path: child_relative,
                    });
                }
            } else if file_type.is_symlink() {
                let target = std::fs::read_link(&source_path)
                    .map_err(|error| snapshot_io("read local source symlink", error))?;
                validate_relative_symlink(&child_relative, &target)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &destination)
                    .map_err(|error| snapshot_io("copy local source symlink", error))?;
                #[cfg(not(unix))]
                return Err(LocalSnapshotError::UnsupportedFileType {
                    path: child_relative,
                });
            } else {
                return Err(LocalSnapshotError::UnsupportedFileType {
                    path: child_relative,
                });
            }
        }
    }
    Ok(())
}

fn digest_snapshot(root: &Path) -> Result<LocalSnapshotDigest, LocalSnapshotError> {
    let mut paths = Vec::new();
    collect_snapshot_paths(root, Path::new(""), &mut paths)?;
    paths.sort_by(|left, right| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    });
    let mut hasher = Sha256::new();
    frame(&mut hasher, SNAPSHOT_DIGEST_VERSION);
    for relative in paths {
        let relative_text = relative
            .to_str()
            .ok_or(LocalSnapshotError::NonPortablePath)?;
        let path = root.join(&relative);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| snapshot_io("inspect staged snapshot", error))?;
        frame(&mut hasher, relative_text.as_bytes());
        if metadata.file_type().is_dir() {
            frame(&mut hasher, b"directory");
        } else if metadata.file_type().is_symlink() {
            frame(&mut hasher, b"symlink");
            let target = std::fs::read_link(&path)
                .map_err(|error| snapshot_io("read staged symlink", error))?;
            frame(&mut hasher, target.as_os_str().as_encoded_bytes());
        } else if metadata.file_type().is_file() {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            frame(&mut hasher, if executable { b"file+x" } else { b"file" });
            let mut file = std::fs::File::open(&path)
                .map_err(|error| snapshot_io("open staged file", error))?;
            hasher.update(metadata.len().to_be_bytes());
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| snapshot_io("read staged file", error))?;
                if read == 0 {
                    break;
                }
                let Some(bytes) = buffer.get(..read) else {
                    return Err(LocalSnapshotError::DigestInvariant);
                };
                hasher.update(bytes);
            }
        } else {
            return Err(LocalSnapshotError::UnsupportedFileType { path: relative });
        }
    }
    LocalSnapshotDigest::try_new(format!("sha256:{:x}", hasher.finalize()))
        .map_err(|_| LocalSnapshotError::DigestInvariant)
}

fn collect_snapshot_paths(
    root: &Path,
    relative: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), LocalSnapshotError> {
    for entry in std::fs::read_dir(root.join(relative))
        .map_err(|error| snapshot_io("read staged directory", error))?
    {
        let entry = entry.map_err(|error| snapshot_io("read staged entry", error))?;
        let child = relative.join(entry.file_name());
        paths.push(child.clone());
        if entry
            .file_type()
            .map_err(|error| snapshot_io("inspect staged entry type", error))?
            .is_dir()
        {
            collect_snapshot_paths(root, &child, paths)?;
        }
    }
    Ok(())
}

fn frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn validate_relative_symlink(path: &Path, target: &Path) -> Result<(), LocalSnapshotError> {
    use std::path::Component;
    if target.is_absolute() || target.to_str().is_none() {
        return Err(LocalSnapshotError::EscapingSymlink {
            path: path.to_path_buf(),
        });
    }
    let mut depth = path
        .parent()
        .map_or(0, |parent| parent.components().count());
    for component in target.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {
                depth += usize::from(matches!(component, Component::Normal(_)))
            }
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(LocalSnapshotError::EscapingSymlink {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

fn metadata_changed(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() != after.len() || before.modified().ok() != after.modified().ok()
}

fn digest_directory(digest: &LocalSnapshotDigest) -> &str {
    digest
        .as_str()
        .strip_prefix("sha256:")
        .expect("validated local snapshot digest")
}

fn set_private_directory_sync(path: &Path) -> Result<(), LocalSnapshotError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| snapshot_io("set snapshot directory permissions", error))?;
    }
    Ok(())
}

fn snapshot_io(action: &'static str, error: std::io::Error) -> LocalSnapshotError {
    LocalSnapshotError::Io {
        action,
        message: error.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourcePreparationError {
    #[error(transparent)]
    Git(#[from] GitCheckoutError),
    #[error(transparent)]
    Local(#[from] LocalSnapshotError),
    #[error("prepared local snapshot {digest} is absent")]
    MissingLocalSnapshot { digest: LocalSnapshotDigest },
    #[error("prepared local snapshot digest {actual} does not match requested {expected}")]
    LocalDigestMismatch {
        expected: LocalSnapshotDigest,
        actual: LocalSnapshotDigest,
    },
    #[error("failed to {action}: {message}")]
    Io {
        action: &'static str,
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum LocalSnapshotError {
    #[error("local snapshot source must be a directory")]
    SourceNotDirectory,
    #[error("local snapshot exceeds the 1 GiB byte limit")]
    ByteLimitExceeded,
    #[error("local snapshot exceeds the 100000 entry limit")]
    EntryLimitExceeded,
    #[error("local snapshot contains a non-portable path")]
    NonPortablePath,
    #[error("local snapshot symlink escapes its root: {path:?}")]
    EscapingSymlink { path: PathBuf },
    #[error("local snapshot contains an unsupported file type: {path:?}")]
    UnsupportedFileType { path: PathBuf },
    #[error("local source changed while it was captured: {path:?}")]
    SourceMutated { path: PathBuf },
    #[error("local snapshot digest invariant failed")]
    DigestInvariant,
    #[error("failed to {action}: {message}")]
    Io {
        action: &'static str,
        message: String,
    },
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

    #[tokio::test]
    async fn local_snapshot_is_deterministic_excludes_ployz_and_consumes_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_root = temp.path().join("project");
        let prepared_root = temp.path().join("executor/prepared");
        fs::create_dir_all(source_root.join("src")).expect("source dirs");
        fs::create_dir_all(source_root.join(".ployz/nested")).expect("ployz dir");
        fs::write(source_root.join("src/main.rs"), "fn main() {}\n").expect("source");
        fs::write(source_root.join(".ployz/nested/credential"), "secret").expect("secret");

        let first = stage_local_snapshot(
            source_root.clone(),
            prepared_root.clone(),
            Some(BuildContextPath::try_new("src").expect("subdir")),
        )
        .await
        .expect("first snapshot");
        let second = stage_local_snapshot(
            source_root,
            prepared_root,
            Some(BuildContextPath::try_new("src").expect("subdir")),
        )
        .await
        .expect("second snapshot");
        assert_eq!(first, second);
        let BuildSource::LocalSnapshot { digest, subdir } = first else {
            panic!("local source")
        };
        let workspace_root = temp.path().join("executor");
        let workspace = workspace_root.join("operation/linux-amd64");
        fs::create_dir_all(&workspace).expect("workspace");
        let prepared =
            consume_local_snapshot(&workspace_root, &workspace, &digest, subdir.as_ref())
                .await
                .expect("consume snapshot");
        assert!(prepared.context().ends_with("source/src"));
        assert!(prepared.context().join("main.rs").exists());
        assert!(!workspace.join("source/.ployz").exists());
        assert!(matches!(
            consume_local_snapshot(
                &workspace_root,
                &workspace_root.join("second"),
                &digest,
                subdir.as_ref()
            )
            .await,
            Err(SourcePreparationError::MissingLocalSnapshot { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_snapshot_rejects_symlinks_that_escape_the_source() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let source_root = temp.path().join("project");
        fs::create_dir(&source_root).expect("source root");
        symlink("../secret", source_root.join("escape")).expect("symlink");
        let error = stage_local_snapshot(source_root, temp.path().join("prepared"), None)
            .await
            .expect_err("escaping link rejected");
        assert!(matches!(error, LocalSnapshotError::EscapingSymlink { .. }));
    }
}
