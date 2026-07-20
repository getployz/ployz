use super::*;

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
) -> Result<PreparedBuildSource, SourcePreparationError> {
    ensure_private_directory(workspace)
        .await
        .map_err(SourcePreparationError::Git)?;
    let prepared = workspace_root
        .join("prepared")
        .join(digest_directory(expected));
    if !tokio::fs::try_exists(&prepared)
        .await
        .map_err(|error| SourcePreparationError::Io {
            action: "inspect prepared local snapshot",
            message: error.to_string(),
        })?
    {
        return Err(SourcePreparationError::MissingLocalSnapshot {
            digest: expected.clone(),
        });
    }
    let root = workspace.join("source");
    tokio::fs::rename(&prepared, &root)
        .await
        .map_err(|error| SourcePreparationError::Io {
            action: "consume prepared local snapshot",
            message: error.to_string(),
        })?;
    let root_for_digest = root.clone();
    let actual = tokio::task::spawn_blocking(move || digest_snapshot(&root_for_digest))
        .await
        .map_err(|error| SourcePreparationError::Io {
            action: "join local snapshot verification",
            message: error.to_string(),
        })?
        .map_err(SourcePreparationError::Local)?;
    if &actual != expected {
        return Err(SourcePreparationError::LocalDigestMismatch {
            expected: expected.clone(),
            actual,
        });
    }
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|error| SourcePreparationError::Io {
            action: "canonicalize local snapshot",
            message: error.to_string(),
        })?;
    let context = match subdir {
        Some(subdir) => canonical_descendant(&root, &root.join(subdir.as_str()))
            .map_err(SourcePreparationError::Git)?,
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
