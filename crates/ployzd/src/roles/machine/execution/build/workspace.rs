use super::plan::{BuildAdapterToolchain, BuildToolchain};
use super::runner::{BuildExecutionError, infrastructure, platform_failure};
use ployz_core::operation::BuildPlatformFailure;
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) async fn prepare_workspace(path: &Path) -> Result<(), BuildExecutionError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|error| infrastructure("inspect build workspace", error.to_string()))?
    {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|error| infrastructure("remove old build workspace", error.to_string()))?;
    }
    prepare_private_directory(path).await
}

pub(super) async fn prepare_private_directory(path: &Path) -> Result<(), BuildExecutionError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|error| infrastructure("create build workspace", error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(|error| {
                infrastructure("set build workspace permissions", error.to_string())
            })?;
    }
    Ok(())
}

pub(super) async fn remove_workspace_tree(path: &Path) -> Result<(), BuildExecutionError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|error| infrastructure("inspect build workspace", error.to_string()))?
    {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|error| infrastructure("remove build workspace", error.to_string()))?;
    }
    Ok(())
}

pub(super) async fn clean_failed_workspace<T>(
    workspace: &Path,
    result: Result<T, BuildExecutionError>,
) -> Result<T, BuildExecutionError> {
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            remove_workspace_tree(workspace).await?;
            Err(error)
        }
    }
}

pub(super) async fn verify_helper(toolchain: &BuildToolchain) -> Result<(), BuildExecutionError> {
    let BuildAdapterToolchain::Railpack {
        helper_path,
        helper_sha256,
        ..
    } = &toolchain.adapter
    else {
        return Ok(());
    };
    let path = helper_path.clone();
    let actual = tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|error| infrastructure("join Railpack verification", error.to_string()))??;
    if actual == helper_sha256.as_str() {
        return Ok(());
    }
    let actual = ployz_core::install::InstallSha256Digest::try_new(actual)
        .map_err(|error| infrastructure("parse Railpack helper digest", error.to_string()))?;
    Err(platform_failure(
        BuildPlatformFailure::HelperDigestMismatch {
            expected: helper_sha256.clone(),
            actual,
        },
    ))
}

fn sha256_file(path: &Path) -> Result<String, BuildExecutionError> {
    use std::io::Read;
    let file = std::fs::File::open(path)
        .map_err(|error| infrastructure("open Railpack helper", error.to_string()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| infrastructure("read Railpack helper", error.to_string()))?;
        if read == 0 {
            break;
        }
        let Some(bytes) = buffer.get(..read) else {
            return Err(infrastructure(
                "read Railpack helper",
                "reader returned an impossible byte count",
            ));
        };
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn successful_workspace_is_retained_until_image_ingestion() {
        let workspace = tempfile::tempdir().expect("workspace");
        let blob = workspace.path().join("blob");
        tokio::fs::write(&blob, b"validated OCI blob")
            .await
            .expect("blob");

        clean_failed_workspace(workspace.path(), Ok::<_, BuildExecutionError>(()))
            .await
            .expect("successful build");

        assert!(tokio::fs::try_exists(blob).await.expect("inspect blob"));
    }

    #[tokio::test]
    async fn failed_workspace_is_removed_immediately() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        tokio::fs::create_dir(&workspace).await.expect("workspace");
        tokio::fs::write(workspace.join("credential"), b"secret")
            .await
            .expect("credential");

        assert!(
            clean_failed_workspace::<()>(&workspace, Err(BuildExecutionError::cancelled()))
                .await
                .is_err()
        );

        assert!(
            !tokio::fs::try_exists(workspace)
                .await
                .expect("inspect workspace")
        );
    }
}
