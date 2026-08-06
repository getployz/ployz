//! Keeper-owned local swap and restart effects.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

use ployz_core::roles::PloyzdRole;
use ployz_host_runner::{
    API_UPGRADE_STAGING_DIRECTORY, PloyzdArtifactStore, SupervisorDirectories,
    SystemHostRunnerCommandRunner, migrate_existing_systemd_api_privileges, role_unit_name,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::roles::upgrade::{UpgradeRequest, UpgradeResponse, read_request, write_response};

const UPGRADE_SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct KeeperUpgradeSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl KeeperUpgradeSocket {
    pub(super) async fn bind(path: &Path) -> Result<Self, KeeperUpgradeSocketError> {
        let parent = path
            .parent()
            .ok_or_else(|| KeeperUpgradeSocketError::MissingParent {
                path: path.to_path_buf(),
            })?;
        tokio::fs::create_dir_all(parent).await.map_err(|source| {
            KeeperUpgradeSocketError::CreateParent {
                path: parent.to_path_buf(),
                message: source.to_string(),
            }
        })?;
        remove_stale_socket(path).await?;
        let listener =
            UnixListener::bind(path).map_err(|source| KeeperUpgradeSocketError::Bind {
                path: path.to_path_buf(),
                message: source.to_string(),
            })?;
        set_socket_permissions(path)?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    pub(super) async fn serve(
        self,
        store: PloyzdArtifactStore,
        command_timeout: Duration,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), KeeperUpgradeSocketError> {
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(|source| KeeperUpgradeSocketError::Accept {
                        path: self.path.clone(),
                        message: source.to_string(),
                    })?;
                    handle_connection(stream, &store, command_timeout).await;
                }
            }
        }
    }
}

impl Drop for KeeperUpgradeSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn remove_stale_socket(path: &Path) -> Result<(), KeeperUpgradeSocketError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_socket() => tokio::fs::remove_file(path)
            .await
            .map_err(|source| KeeperUpgradeSocketError::RemoveStaleSocket {
                path: path.to_path_buf(),
                message: source.to_string(),
            }),
        Ok(_) => Err(KeeperUpgradeSocketError::ExistingNonSocket {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(KeeperUpgradeSocketError::Inspect {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> Result<(), KeeperUpgradeSocketError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(|source| KeeperUpgradeSocketError::Inspect {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?
        .permissions();
    permissions.set_mode(0o660);
    std::fs::set_permissions(path, permissions).map_err(|source| {
        KeeperUpgradeSocketError::SetPermissions {
            path: path.to_path_buf(),
            message: source.to_string(),
        }
    })
}

#[cfg(not(unix))]
fn set_socket_permissions(_path: &Path) -> Result<(), KeeperUpgradeSocketError> {
    Ok(())
}

async fn handle_connection(
    mut stream: UnixStream,
    store: &PloyzdArtifactStore,
    command_timeout: Duration,
) {
    let response =
        match tokio::time::timeout(UPGRADE_SOCKET_IO_TIMEOUT, read_request(&mut stream)).await {
            Ok(Ok(request)) => handle_request(request, store, command_timeout).await,
            Ok(Err(error)) => UpgradeResponse::Refused {
                message: error.to_string(),
            },
            Err(_) => UpgradeResponse::Refused {
                message: "Keeper upgrade request timed out".to_owned(),
            },
        };
    if let Err(error) = tokio::time::timeout(
        UPGRADE_SOCKET_IO_TIMEOUT,
        write_response(&mut stream, &response),
    )
    .await
    {
        tracing::warn!(error = ?error, "could not return Keeper upgrade response");
    }
}

async fn handle_request(
    request: UpgradeRequest,
    store: &PloyzdArtifactStore,
    command_timeout: Duration,
) -> UpgradeResponse {
    match request {
        UpgradeRequest::Arm { version, sha256 } => {
            let staging_store =
                match PloyzdArtifactStore::new(store.state().join(API_UPGRADE_STAGING_DIRECTORY)) {
                    Ok(store) => store,
                    Err(error) => {
                        return UpgradeResponse::Refused {
                            message: error.to_string(),
                        };
                    }
                };
            match store
                .adopt_upgrade_candidate(&staging_store, &version, &sha256)
                .and_then(|_| store.arm_staged(&sha256))
            {
                Ok(_) => UpgradeResponse::Armed,
                Err(error) => UpgradeResponse::Refused {
                    message: error.to_string(),
                },
            }
        }
        UpgradeRequest::Commit => match store.pending_upgrade() {
            Ok(Some(_)) => match restart_systemd_role(PloyzdRole::Keeper, command_timeout).await {
                Ok(()) => UpgradeResponse::Committed,
                Err(message) => UpgradeResponse::Refused { message },
            },
            Ok(None) => UpgradeResponse::Refused {
                message: "no armed ployzd upgrade is pending".to_owned(),
            },
            Err(error) => UpgradeResponse::Refused {
                message: error.to_string(),
            },
        },
    }
}

pub(super) async fn restart_systemd_role(
    role: PloyzdRole,
    timeout: Duration,
) -> Result<(), String> {
    let unit = role_unit_name(&role);
    let mut command = tokio::process::Command::new("/usr/bin/systemctl");
    command
        .args(["--no-block", "restart", unit.as_str()])
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("systemctl restart {unit} timed out after {timeout:?}"))?
        .map_err(|error| format!("could not run systemctl restart {unit}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl restart {unit} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub(super) async fn migrate_api_privileges(
    state: PathBuf,
    command_timeout: Duration,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let mut runner = SystemHostRunnerCommandRunner::new(command_timeout);
        migrate_existing_systemd_api_privileges(
            &state,
            &SupervisorDirectories::host_defaults(),
            &mut runner,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("API privilege migration task failed: {error}"))?
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum KeeperUpgradeSocketError {
    #[error("Keeper upgrade socket path {path} has no parent directory")]
    MissingParent { path: PathBuf },
    #[error("could not create Keeper upgrade socket directory {path}: {message}")]
    CreateParent { path: PathBuf, message: String },
    #[error("could not inspect Keeper upgrade socket {path}: {message}")]
    Inspect { path: PathBuf, message: String },
    #[error("Keeper upgrade socket path {path} already exists and is not a socket")]
    ExistingNonSocket { path: PathBuf },
    #[error("could not remove stale Keeper upgrade socket {path}: {message}")]
    RemoveStaleSocket { path: PathBuf, message: String },
    #[error("could not bind Keeper upgrade socket {path}: {message}")]
    Bind { path: PathBuf, message: String },
    #[error("could not set Keeper upgrade socket permissions {path}: {message}")]
    SetPermissions { path: PathBuf, message: String },
    #[error("could not accept Keeper upgrade socket connection {path}: {message}")]
    Accept { path: PathBuf, message: String },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ployz_core::install::{InstallArtifactVersion, InstallSha256Digest};
    use ployz_host_runner::verify_artifact_file;

    use super::*;

    #[test]
    fn keeper_restart_uses_the_systemd_role_unit() {
        assert_eq!(role_unit_name(&PloyzdRole::Keeper), "ployzd-keeper.service");
    }

    #[tokio::test]
    async fn arm_request_reverifies_and_adopts_api_staging_before_arming_live_store() {
        let root = tempfile::tempdir().expect("upgrade root");
        let state = root.path().join("state");
        let live = PloyzdArtifactStore::new(state.clone()).expect("live store");
        let old_source = root.path().join("old");
        let candidate_source = root.path().join("candidate");
        fs::write(&old_source, b"old\n").expect("old bytes");
        fs::write(&candidate_source, b"new\n").expect("candidate bytes");
        let old_digest = InstallSha256Digest::try_new(
            "01d09d19c2139a46aebfb577780d123d7396e97201bc7ead210a2ebff8239dee",
        )
        .expect("old digest");
        let candidate_digest = InstallSha256Digest::try_new(
            "7aa7a5359173d05b63cfd682e3c38487f3cb4f7f1d60659fe59fab1505977d4c",
        )
        .expect("candidate digest");
        let old = verify_artifact_file(&old_source, &old_digest).expect("old verifies");
        live.seed_current(&old).expect("live current seeds");
        let candidate =
            verify_artifact_file(&candidate_source, &candidate_digest).expect("candidate verifies");
        let staging = PloyzdArtifactStore::new(state.join(API_UPGRADE_STAGING_DIRECTORY))
            .expect("staging store");
        let version = InstallArtifactVersion::try_new("1.2.3").expect("version");
        staging
            .stage_upgrade_candidate(&candidate, &version)
            .expect("API candidate stages");

        assert_eq!(
            handle_request(
                UpgradeRequest::Arm {
                    version,
                    sha256: candidate_digest.clone(),
                },
                &live,
                Duration::from_secs(1),
            )
            .await,
            UpgradeResponse::Armed
        );
        assert!(
            live.artifacts_path()
                .join(candidate_digest.as_str())
                .exists()
        );
        assert_eq!(
            live.pending_upgrade().expect("pending marker"),
            Some(candidate_digest)
        );
    }
}
