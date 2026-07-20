use super::PreparedBuildSource;
use ployz_core::build::{BuildContextPath, BuildSource, LocalSnapshotDigest, VerifiedBuildSource};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_SNAPSHOT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SNAPSHOT_ENTRIES: usize = 100_000;
const SNAPSHOT_DIGEST_VERSION: &[u8] = b"ployz.local-snapshot.v1";

#[derive(Clone, Copy)]
struct SnapshotLimits {
    bytes: u64,
    entries: usize,
}

const SNAPSHOT_LIMITS: SnapshotLimits = SnapshotLimits {
    bytes: MAX_SNAPSHOT_BYTES,
    entries: MAX_SNAPSHOT_ENTRIES,
};

pub(crate) async fn stage_local_snapshot(
    source_root: PathBuf,
    prepared_root: PathBuf,
    subdir: Option<BuildContextPath>,
) -> Result<BuildSource, LocalSnapshotError> {
    tokio::task::spawn_blocking(move || {
        stage_local_snapshot_blocking(&source_root, &prepared_root, subdir)
    })
    .await
    .map_err(|error| LocalSnapshotError::Io {
        action: "join local snapshot preparation",
        message: error.to_string(),
    })?
}

pub(super) async fn consume_local_snapshot(
    workspace_root: &Path,
    workspace: &Path,
    expected: &LocalSnapshotDigest,
    subdir: Option<&BuildContextPath>,
) -> Result<PreparedBuildSource, LocalSnapshotPreparationError> {
    ensure_private_directory(workspace).await?;
    let prepared = workspace_root
        .join("prepared")
        .join(digest_directory(expected));
    if !tokio::fs::try_exists(&prepared)
        .await
        .map_err(|error| preparation_io("inspect prepared local snapshot", error))?
    {
        return Err(LocalSnapshotPreparationError::Missing {
            digest: expected.clone(),
        });
    }
    let root = workspace.join("source");
    tokio::fs::rename(&prepared, &root)
        .await
        .map_err(|error| preparation_io("consume prepared local snapshot", error))?;
    let root_for_digest = root.clone();
    let actual = tokio::task::spawn_blocking(move || digest_snapshot(&root_for_digest))
        .await
        .map_err(|error| LocalSnapshotPreparationError::Io {
            action: "join local snapshot verification",
            message: error.to_string(),
        })??;
    if &actual != expected {
        return Err(LocalSnapshotPreparationError::DigestMismatch {
            expected: expected.clone(),
            actual,
        });
    }
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|error| preparation_io("canonicalize local snapshot", error))?;
    let context = match subdir {
        Some(subdir) => local_descendant(&root, &root.join(subdir.as_str()))?,
        None => root,
    };
    Ok(PreparedBuildSource {
        context,
        verified: VerifiedBuildSource::LocalSnapshot {
            digest: expected.clone(),
            subdir: subdir.cloned(),
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
    copy_snapshot_tree_with_limits(source_root, destination_root, SNAPSHOT_LIMITS)
}

fn copy_snapshot_tree_with_limits(
    source_root: &Path,
    destination_root: &Path,
    limits: SnapshotLimits,
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
            if entry_count > limits.entries {
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
                if copied_bytes > limits.bytes {
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

async fn ensure_private_directory(path: &Path) -> Result<(), LocalSnapshotPreparationError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|error| preparation_io("create local snapshot workspace", error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| preparation_io("set local snapshot workspace permissions", error))?;
    }
    Ok(())
}

fn local_descendant(
    root: &Path,
    candidate: &Path,
) -> Result<PathBuf, LocalSnapshotPreparationError> {
    let root = std::fs::canonicalize(root)
        .map_err(|error| preparation_io("canonicalize local snapshot root", error))?;
    let candidate = std::fs::canonicalize(candidate)
        .map_err(|error| preparation_io("canonicalize local snapshot build path", error))?;
    if !candidate.starts_with(&root) {
        return Err(LocalSnapshotPreparationError::PathEscapesSnapshot { candidate });
    }
    Ok(candidate)
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

fn preparation_io(action: &'static str, error: std::io::Error) -> LocalSnapshotPreparationError {
    LocalSnapshotPreparationError::Io {
        action,
        message: error.to_string(),
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LocalSnapshotPreparationError {
    #[error(transparent)]
    Snapshot(#[from] LocalSnapshotError),
    #[error("prepared local snapshot {digest} is absent")]
    Missing { digest: LocalSnapshotDigest },
    #[error("prepared local snapshot digest {actual} does not match requested {expected}")]
    DigestMismatch {
        expected: LocalSnapshotDigest,
        actual: LocalSnapshotDigest,
    },
    #[error("build path escapes the local snapshot: {candidate:?}")]
    PathEscapesSnapshot { candidate: PathBuf },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    async fn stage_and_consume(
        source_root: PathBuf,
        executor_root: &Path,
    ) -> (BuildSource, PreparedBuildSource) {
        let source = stage_local_snapshot(source_root, executor_root.join("prepared"), None)
            .await
            .expect("stage snapshot");
        let BuildSource::LocalSnapshot { digest, subdir } = &source else {
            panic!("local source")
        };
        let workspace = executor_root.join("operation/linux-amd64");
        let prepared = consume_local_snapshot(executor_root, &workspace, digest, subdir.as_ref())
            .await
            .expect("consume snapshot");
        (source, prepared)
    }

    #[tokio::test]
    async fn snapshot_excludes_executor_material_without_injecting_source_configuration() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_root = temp.path().join("project");
        let executor_root = temp.path().join("executor");
        fs::create_dir_all(source_root.join(".git")).expect("git dir");
        fs::create_dir_all(source_root.join(".ployz/generated")).expect("ployz dir");
        fs::create_dir_all(source_root.join("src")).expect("source dir");
        fs::write(source_root.join(".git/config"), "credential=git-secret").expect("git config");
        fs::write(
            source_root.join(".ployz/generated/supplied-secret"),
            "generated-secret-sentinel",
        )
        .expect("generated secret");
        fs::write(
            source_root.join(".ployz/generated/source-config"),
            "source-config-sentinel",
        )
        .expect("source config");
        fs::write(source_root.join("src/main.rs"), "fn main() {}\n").expect("source");

        let (_, prepared) = stage_and_consume(source_root, &executor_root).await;
        assert!(prepared.context().join("src/main.rs").exists());
        assert!(!prepared.context().join(".git").exists());
        assert!(!prepared.context().join(".ployz").exists());
        let captured =
            fs::read_to_string(prepared.context().join("src/main.rs")).expect("captured source");
        assert!(!captured.contains("git-secret"));
        assert!(!captured.contains("generated-secret-sentinel"));
        assert!(!captured.contains("source-config-sentinel"));
    }

    #[tokio::test]
    async fn ordinary_uncommitted_content_changes_digest_and_survives_consumption() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_root = temp.path().join("project");
        fs::create_dir_all(&source_root).expect("source root");
        fs::write(source_root.join("ordinary.txt"), "working tree one").expect("ordinary source");
        let (first, first_prepared) =
            stage_and_consume(source_root.clone(), &temp.path().join("first")).await;
        assert_eq!(
            fs::read_to_string(first_prepared.context().join("ordinary.txt"))
                .expect("captured ordinary source"),
            "working tree one"
        );

        fs::write(source_root.join("ordinary.txt"), "working tree two")
            .expect("mutated ordinary source");
        let (second, second_prepared) =
            stage_and_consume(source_root, &temp.path().join("second")).await;
        assert_ne!(first, second);
        assert_eq!(
            fs::read_to_string(second_prepared.context().join("ordinary.txt"))
                .expect("captured mutated source"),
            "working tree two"
        );
    }

    #[tokio::test]
    async fn identical_snapshot_is_deterministic_and_consumes_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_root = temp.path().join("project");
        let executor_root = temp.path().join("executor");
        fs::create_dir_all(source_root.join("src")).expect("source dir");
        fs::write(source_root.join("src/main.rs"), "fn main() {}\n").expect("source");
        let subdir = Some(BuildContextPath::try_new("src").expect("subdir"));

        let first = stage_local_snapshot(
            source_root.clone(),
            executor_root.join("prepared"),
            subdir.clone(),
        )
        .await
        .expect("first snapshot");
        let second = stage_local_snapshot(source_root, executor_root.join("prepared"), subdir)
            .await
            .expect("second snapshot");
        assert_eq!(first, second);

        let BuildSource::LocalSnapshot { digest, subdir } = first else {
            panic!("local source")
        };
        let first_workspace = executor_root.join("operation/linux-amd64");
        let prepared =
            consume_local_snapshot(&executor_root, &first_workspace, &digest, subdir.as_ref())
                .await
                .expect("consume snapshot");
        assert!(prepared.context().ends_with("source/src"));
        assert!(prepared.context().join("main.rs").exists());
        assert!(matches!(
            consume_local_snapshot(
                &executor_root,
                &executor_root.join("operation/second"),
                &digest,
                subdir.as_ref(),
            )
            .await,
            Err(LocalSnapshotPreparationError::Missing { .. })
        ));
    }

    #[tokio::test]
    async fn invalid_local_subdir_reports_local_preparation_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_root = temp.path().join("project");
        let executor_root = temp.path().join("executor");
        fs::create_dir(&source_root).expect("source root");
        fs::write(source_root.join("main.rs"), "fn main() {}\n").expect("source");
        let source = stage_local_snapshot(
            source_root,
            executor_root.join("prepared"),
            Some(BuildContextPath::try_new("missing").expect("subdir")),
        )
        .await
        .expect("stage snapshot");
        let BuildSource::LocalSnapshot { digest, subdir } = source else {
            panic!("local source")
        };

        assert!(matches!(
            consume_local_snapshot(
                &executor_root,
                &executor_root.join("operation/linux-amd64"),
                &digest,
                subdir.as_ref(),
            )
            .await,
            Err(LocalSnapshotPreparationError::Io {
                action: "canonicalize local snapshot build path",
                ..
            })
        ));
    }

    #[test]
    fn snapshot_copy_enforces_byte_and_entry_limits() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        fs::create_dir(&source_root).expect("source root");
        fs::write(source_root.join("a"), "1234").expect("first file");
        fs::write(source_root.join("b"), "5").expect("second file");

        let byte_destination = temp.path().join("byte-destination");
        fs::create_dir(&byte_destination).expect("byte destination");
        assert!(matches!(
            copy_snapshot_tree_with_limits(
                &source_root,
                &byte_destination,
                SnapshotLimits {
                    bytes: 4,
                    entries: 2,
                },
            ),
            Err(LocalSnapshotError::ByteLimitExceeded)
        ));

        let entry_destination = temp.path().join("entry-destination");
        fs::create_dir(&entry_destination).expect("entry destination");
        assert!(matches!(
            copy_snapshot_tree_with_limits(
                &source_root,
                &entry_destination,
                SnapshotLimits {
                    bytes: 5,
                    entries: 1,
                },
            ),
            Err(LocalSnapshotError::EntryLimitExceeded)
        ));
    }

    #[test]
    fn mutation_detection_compares_source_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        fs::write(&source, "before").expect("source");
        let before = fs::metadata(&source).expect("before metadata");
        fs::write(&source, "after content").expect("mutate source");
        let after = fs::metadata(&source).expect("after metadata");
        assert!(metadata_changed(&before, &after));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_rejects_symlinks_that_escape_the_source() {
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
